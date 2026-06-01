//! Plain-text debug UART (USART3 TX on D2 / PC10) + panic handler.
//!
//! Why plain-text-over-UART and not defmt-serial or USB-serial:
//!   - defmt-serial pins `defmt = "0.3"`, incompatible with our defmt 1.0.
//!   - USB-serial can't emit panics (the executor halts before they drain).
//! So debug output is plain ASCII over a dedicated UART, read on the Shikra at
//! 115200. defmt-rtt stays the defmt global logger (existing `info!` calls go
//! to RTT, unread without a probe — harmless). USART3 keeps this independent
//! of USART1/MIDI so the baud rates can never collide.

use core::cell::RefCell;
use core::fmt::Write;

use embassy_stm32::mode::Blocking;
use embassy_stm32::usart::UartTx;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

pub type DebugTx = UartTx<'static, Blocking>;

static DEBUG_TX: Mutex<CriticalSectionRawMutex, RefCell<Option<DebugTx>>> =
    Mutex::new(RefCell::new(None));

/// Install the UART so `dbg_uart!` and the panic handler can write to it.
pub fn init(tx: DebugTx) {
    DEBUG_TX.lock(|c| *c.borrow_mut() = Some(tx));
}

struct Writer<'a>(&'a mut DebugTx);

impl Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0
            .blocking_write(s.as_bytes())
            .map_err(|_| core::fmt::Error)
    }
}

/// Backing fn for `dbg_uart!`. Blocking — call from setup / the reader, never
/// from the audio callback (a blocking write there would stall the real-time
/// path and cause the very overruns we're chasing).
pub fn write_fmt(args: core::fmt::Arguments) {
    DEBUG_TX.lock(|c| {
        if let Ok(mut g) = c.try_borrow_mut() {
            if let Some(tx) = g.as_mut() {
                let mut w = Writer(tx);
                let _ = write!(w, "{}\r\n", args);
            }
        }
    });
}

#[macro_export]
macro_rules! dbg_uart {
    ($($a:tt)*) => { $crate::debug::write_fmt(format_args!($($a)*)) };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // The whole reason for the custom handler: synchronously print the panic
    // (location + message) so it lands on the Shikra even though the executor
    // is dead. try_borrow guards against a panic that fired mid-write.
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
