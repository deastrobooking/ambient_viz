//! SD card stack: SPI1 + embedded-sdmmc.
//!
//! Hardware: WWZMDiB 6-pin microSD module on Daisy SPI1. See daisy/BREAKOUT.md.
//!
//!     Module pin  Daisy pad  STM32 pin   Function
//!     VCC         pad 38     —           +3V3
//!     GND         pad 40     —           GND
//!     MISO        D9         PB4         SPI1_MISO
//!     MOSI        D10        PB5         SPI1_MOSI
//!     SCK         D8         PG11        SPI1_SCK
//!     CS          D7         PG10        software-driven GPIO (idle high)
//!
//! This module currently exposes only the construction path so we can
//! compile-check the integration before the physical breakout is built.
//! Actual mount + read happens during hardware bring-up.
#![allow(dead_code)]

use daisy_embassy::pins::{SeedPin7, SeedPin8, SeedPin9, SeedPin10};
use embassy_stm32::Peri;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::mode::Blocking;
use embassy_stm32::peripherals::SPI1;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{SdCard, TimeSource, Timestamp};

/// Concrete blocking-SPI device wrapping SPI1 + a GPIO CS, ready to pass to
/// `SdCard::new`.
pub type SdSpi<'a> = ExclusiveDevice<Spi<'a, Blocking, Master>, Output<'a>, Delay>;

/// SPI clock for the SD init handshake. The SD spec mandates ≤400 kHz for
/// CMD0/ACMD41, and slow edges are far more reliable on the crowded hand-wired
/// breakout — initialising at full speed is the usual cause of intermittent
/// "card not found" on cold boot.
const INIT_HZ: u32 = 400_000;
/// SPI clock for streaming once the card is acquired (SD default-speed ≤25 MHz).
const FAST_HZ: u32 = 24_000_000;

/// Build the SPI1 bus + CS pin into the SpiDevice that embedded-sdmmc wants,
/// then wrap in an `SdCard`. Does NOT initialise the card — that happens
/// lazily on the first block-device call (e.g. `num_bytes()`), which will
/// block until a card is actually present.
///
/// Peripherals are passed in as already-wrapped `Peri<'_, T>` because that's
/// what `board.pins.dN` and `p.SPI1` return after `new_daisy_board!`.
pub fn build_sd_card<'a>(
    spi1: Peri<'a, SPI1>,
    sck: SeedPin8<'a>,
    mosi: SeedPin10<'a>,
    miso: SeedPin9<'a>,
    cs: SeedPin7<'a>,
) -> SdCard<SdSpi<'a>, Delay> {
    // Start slow for a reliable init handshake; `set_fast` bumps to FAST_HZ once
    // the card is acquired. (Audio is decoupled by the 8192-sample ring on its
    // own executor, so streaming-clock choice no longer gates the audio deadline.)
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz(INIT_HZ);

    let spi = Spi::new_blocking(spi1, sck, mosi, miso, spi_config);

    // CS is software-driven, idle high.
    let cs = Output::new(cs, Level::High, Speed::Low);

    // ExclusiveDevice ties bus + CS together for embedded-hal-1.0 SpiDevice.
    // The Delay parameter is used by the trait to space out CS pulses; we
    // hand it an embassy-time Delay impl.
    let spi_device = ExclusiveDevice::new(spi, cs, Delay).unwrap();

    SdCard::new(spi_device, Delay)
}

/// Bump the SD SPI clock to streaming speed. Call only after the card has been
/// acquired at `INIT_HZ` — the card stays initialised across the reconfigure;
/// only the bus baud rate changes.
pub fn set_fast(sdcard: &SdCard<SdSpi<'_>, Delay>) {
    let mut cfg = SpiConfig::default();
    cfg.frequency = Hertz(FAST_HZ);
    sdcard.spi(|dev| {
        let _ = dev.bus_mut().set_config(&cfg);
    });
}

/// Stub TimeSource. embedded-sdmmc uses this to stamp FAT directory entries
/// on writes; reads don't need it. When we add real wall-clock awareness
/// (probably from a MIDI timecode message or USB SOF), wire it in here.
pub struct ZeroTime;

impl TimeSource for ZeroTime {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}
