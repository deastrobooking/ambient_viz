//! USB device plumbing for the UAC1 audio source (capture) over OTG-FS.
//!
//! Stage 1 streams silence to validate enumeration + the iso-IN endpoint on the
//! host. Stage 2 will tee the real audio samples in. The `embassy_usb::Builder`
//! wiring lives inline in `main` (it needs the board peripherals + StaticCells);
//! this module holds the interrupt binding, constants, and the device/stream
//! tasks (which need the concrete driver type).

use core::sync::atomic::Ordering;

use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::UsbDevice;
use heapless::spsc::Consumer;

use crate::uac_source::{AudioSourceEpIn, MAX_EXTRA_SAMPLES};
use crate::USB_RING_LEN;
use crate::{USB_CAPTURING, USB_PKT_MAX_FR};

bind_interrupts!(pub struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

pub const SAMPLE_RATE_HZ: u32 = 48_000;
pub static SAMPLE_RATES: [u32; 1] = [SAMPLE_RATE_HZ];
pub const CHANNELS: usize = 2;
/// 16-bit samples == SampleWidth::Width2Byte (matches our i16 stream).
pub const SAMPLE_BYTES: usize = 2;
/// Bytes of audio per 1 ms USB frame (one iso-IN packet).
pub const FRAME_BYTES: usize = (SAMPLE_RATE_HZ as usize) * CHANNELS * SAMPLE_BYTES / 1000;
/// Feedback refresh period in (full-speed) ms.
pub const FEEDBACK_REFRESH_MS: u8 = 8;
/// Max iso-IN packet: nominal frame + async headroom. Must match the endpoint's
/// declared wMaxPacketSize in `uac_source::AudioSource::new`.
pub const MAX_PACKET_BYTES: usize = FRAME_BYTES + CHANNELS * SAMPLE_BYTES * MAX_EXTRA_SAMPLES as usize;
/// OTG-FS shared OUT buffer. A source has no audio OUT endpoint — only EP0
/// control is OUT — so this only needs to cover the control endpoint.
pub const EP_OUT_BUF: usize = 256;

pub type Drv = usb::Driver<'static, peripherals::USB_OTG_FS>;

/// Runs the USB device (enumeration, control transfers).
#[embassy_executor::task]
pub async fn usb_task(mut dev: UsbDevice<'static, Drv>) {
    crate::dbg_uart!("usb: run() task polled — bus coming up");
    dev.run().await;
}

/// Stage 2: drain the SAI tee ring into the iso-IN endpoint. Each poll we send
/// whatever's queued (up to the endpoint max), which lets a packet "catch up"
/// after a host poll we missed while the thread executor was blocked on an SD
/// read — without that catch-up the stream falls permanently behind. `write`
/// blocks until the host polls, pacing this to the 1 ms SOF.
#[embassy_executor::task]
pub async fn stream_task(
    mut audio_ep: AudioSourceEpIn<'static, Drv>,
    mut samples: Consumer<'static, i16, USB_RING_LEN>,
) {
    let mut pkt = [0u8; MAX_PACKET_BYTES];
    loop {
        audio_ep.wait_enabled().await;
        crate::dbg_uart!("uac: audio IN enabled — streaming line-out");
        // Reset latency: drop the backlog buffered while the host wasn't capturing.
        while samples.dequeue().is_some() {}
        // Now capturing: arm the tee's drop counter (see USB_DROP in main.rs).
        USB_CAPTURING.store(true, Ordering::Relaxed);
        loop {
            let mut len = 0;
            while len + 2 * SAMPLE_BYTES <= pkt.len() {
                let Some(l) = samples.dequeue() else { break };
                let r = samples.dequeue().unwrap_or(0);
                pkt[len..len + 2].copy_from_slice(&l.to_le_bytes());
                pkt[len + 2..len + 4].copy_from_slice(&r.to_le_bytes());
                len += 4;
            }
            // DIAG: peak single-poll drain in stereo frames. ~48 = healthy 1 ms
            // pacing; toward the 56-frame cap = catching up after missed polls.
            USB_PKT_MAX_FR.fetch_max((len / (2 * SAMPLE_BYTES)) as u32, Ordering::Relaxed);
            if audio_ep.write(&pkt[..len]).await.is_err() {
                crate::dbg_uart!("uac: audio IN disabled");
                USB_CAPTURING.store(false, Ordering::Relaxed);
                break;
            }
        }
    }
}
