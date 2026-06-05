//! Plain-text debug UART (USART3 TX on D2 / PC10) + panic handler.
//!
//! Why plain-text-over-UART and not defmt-serial or USB-serial:
//!   - defmt-serial pins `defmt = "0.3"`, incompatible with our defmt 1.0.
//!   - USB-serial can't emit panics (the executor halts before they drain).
//! So debug output is plain ASCII over a dedicated UART, read on the Shikra at
//! 115200. defmt-rtt stays the defmt global logger (existing `info!` calls go
//! to RTT, unread without a probe — harmless). USART3 keeps this independent
//! of USART1/MIDI so the baud rates can never collide.
//!
//! Build-time gating: the whole subsystem sits behind the `debug-uart` feature
//! (on by default). With it off — the production build — `dbg_uart!` expands to
//! nothing, the UART is never brought up, and the panic handler just halts. So
//! shipping firmware emits no UART debug traffic and doesn't link the writer.

/// Emit a line over the debug UART, real-time-safely (per-byte critical section,
/// see `write_fmt`). No-op unless built with the `debug-uart` feature — in that
/// case the arguments are still type-checked but never evaluated/formatted.
#[cfg(feature = "debug-uart")]
#[macro_export]
macro_rules! dbg_uart {
    ($($a:tt)*) => { $crate::debug::write_fmt(format_args!($($a)*)) };
}

#[cfg(not(feature = "debug-uart"))]
#[macro_export]
macro_rules! dbg_uart {
    ($($a:tt)*) => {{
        // Reference the args in a never-called closure: this type-checks the
        // format string and marks every captured variable "used" (no spurious
        // warnings), but the body never runs, so the argument expressions are
        // never evaluated and the whole thing optimises to nothing.
        let _ = || { let _ = ::core::format_args!($($a)*); };
    }};
}

// ---------------------------------------------------------------------------
// `debug-uart` ON: real UART writer + panic message.
// ---------------------------------------------------------------------------
#[cfg(feature = "debug-uart")]
mod imp {
    use core::cell::RefCell;
    use core::fmt::Write;

    use embassy_stm32::mode::Blocking;
    use embassy_stm32::usart::UartTx;
    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

    pub type DebugTx = UartTx<'static, Blocking>;

    pub(super) static DEBUG_TX: Mutex<CriticalSectionRawMutex, RefCell<Option<DebugTx>>> =
        Mutex::new(RefCell::new(None));

    /// Install the UART so `dbg_uart!` and the panic handler can write to it.
    pub fn init(tx: DebugTx) {
        DEBUG_TX.lock(|c| *c.borrow_mut() = Some(tx));
    }

    /// Real-time-safe writer: takes the UART critical section **one byte at a
    /// time** rather than holding it across the whole (blocking) line. Each byte
    /// masks interrupts only ~one character-time (~87 µs at 115200, well under
    /// the 0.67 ms SAI budget), and the audio interrupt executor runs between
    /// bytes — so logging from the thread executor can no longer starve the
    /// audio. (A whole-line CS was masking ~6 ms and underrunning the SAI.)
    struct ByteWriter;

    impl Write for ByteWriter {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                DEBUG_TX.lock(|c| {
                    if let Ok(mut g) = c.try_borrow_mut() {
                        if let Some(tx) = g.as_mut() {
                            let _ = tx.blocking_write(&[b]);
                        }
                    }
                });
            }
            Ok(())
        }
    }

    /// Backing fn for `dbg_uart!`. Real-time-safe (per-byte CS) so it can be
    /// called from the thread executor while audio runs — but still never from
    /// the audio callback itself.
    pub fn write_fmt(args: core::fmt::Arguments) {
        let mut w = ByteWriter;
        let _ = write!(w, "{}\r\n", args);
    }

    /// Whole-line writer used only by the panic handler (the executor is already
    /// dead, so the per-byte RT discipline no longer matters).
    struct Writer<'a>(&'a mut DebugTx);

    impl Write for Writer<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0
                .blocking_write(s.as_bytes())
                .map_err(|_| core::fmt::Error)
        }
    }

    #[panic_handler]
    fn panic(info: &core::panic::PanicInfo) -> ! {
        // The whole reason for the custom handler: synchronously print the panic
        // (location + message) so it lands on the Shikra even though the executor
        // is dead. try_borrow guards against a panic that fired mid-write.
        //
        // NB: this does NOT meaningfully affect flash size. Formatting floats
        // (flt2dec, ~10 KB) is pulled into the debug-uart build by a separate
        // core::fmt path, not by this handler — printing location-only here was
        // tried and changed nothing. See PLAN_QSPI_BOOTLOADER.md.
        DEBUG_TX.lock(|c| {
            if let Ok(mut g) = c.try_borrow_mut() {
                if let Some(tx) = g.as_mut() {
                    let mut w = Writer(tx);
                    let _ = write!(w, "\r\n*** PANIC: {} ***\r\n", info);
                }
            }
        });
        loop {
            cortex_m::asm::udf();
        }
    }
}

#[cfg(feature = "debug-uart")]
pub use imp::{init, write_fmt};

// ---------------------------------------------------------------------------
// `debug-uart` OFF (production): no UART, and in the field no probe either, so
// make panics VISIBLE on the user LED. We STROBE PC7 (active-high) fast and
// forever — clearly distinct from the 500 ms boot heartbeat and the 1/2/3 SD
// blink codes, so "fast strobe" unambiguously means *the firmware panicked*
// (vs a HardFault/hang, which leaves the LED frozen/dark since this handler
// never runs). Raw GPIOC BSRR is used because a panic handler can't own the
// UserLed; PC7 is configured as output very early in main (board init) before
// anything that can panic here, so the port is clocked and the writes land.
// A defmt marker is also emitted for the case a probe IS attached (cargo
// flash-prod); it's a fixed string interned in the non-loaded `.defmt` section,
// so it costs ~0 flash and avoids the ~25 KB `info.location()` retention.
// ---------------------------------------------------------------------------
#[cfg(not(feature = "debug-uart"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    defmt::error!("*** PANIC (prod) — LED strobe on PC7 ***");
    use embassy_stm32::pac::GPIOC;
    loop {
        GPIOC.bsrr().write(|w| w.set_bs(7, true)); // PC7 high → LED on
        cortex_m::asm::delay(40_000_000); // ~80 ms @ 480 MHz
        GPIOC.bsrr().write(|w| w.set_br(7, true)); // PC7 low → LED off
        cortex_m::asm::delay(40_000_000);
    }
}
