//! Periodic STM32H750 die-temperature readout over the debug UART.
//!
//! The H7 has an internal temperature sensor on ADC3 input 18 (VSENSE). We read
//! it every ~5 s from the *thread* executor — i.e. below the audio interrupt
//! executor (UART4/P6) and USB (OTG/P7) — so the conversion's short busy-wait
//! (~20 µs at 810.5-cycle sampling) is freely preempted by the audio callback
//! and can't underrun the SAI. The print itself is the RT-safe per-byte
//! `dbg_uart!`, and the whole task is compiled out without the `debug-uart`
//! feature.
//!
//! Conversion uses the factory linear calibration (RM0433 §25.4.31 / DS12930):
//! two raw ADC counts taken at 3.3 V VDDA and stored in system memory —
//! TS_CAL1 @ 30 °C, TS_CAL2 @ 110 °C. We interpolate between them. This assumes
//! VDDA ≈ 3.3 V (true on the Daisy); we don't VREFINT-compensate, so treat the
//! number as ±a few °C — fine for "is the board cooking?" telemetry.

use embassy_stm32::Peri;
use embassy_stm32::adc::{Adc, AdcConfig, Resolution, SampleTime};
use embassy_stm32::peripherals::ADC3;
use embassy_time::Timer;

/// Raw 16-bit ADC count from the temp sensor at 30 °C, VDDA = 3.3 V (factory).
const TS_CAL1_ADDR: *const u16 = 0x1FF1_E820 as *const u16;
/// Raw 16-bit ADC count from the temp sensor at 110 °C, VDDA = 3.3 V (factory).
const TS_CAL2_ADDR: *const u16 = 0x1FF1_E840 as *const u16;
const TS_CAL1_TEMP_C: f32 = 30.0;
const TS_CAL2_TEMP_C: f32 = 110.0;

const PERIOD_SECS: u64 = 5;

#[embassy_executor::task]
pub async fn temp_task(adc3: Peri<'static, ADC3>) {
    // Cal counts are 16-bit; match the conversion resolution so the slope lines
    // up (reset default is already 16-bit, but be explicit).
    let mut adc = Adc::new_with_config(
        adc3,
        AdcConfig {
            resolution: Some(Resolution::BITS16),
            averaging: None,
        },
    );
    let mut ts = adc.enable_temperature();

    // Slope from the two factory points. Guard the (impossible-on-real-silicon)
    // degenerate case where both reads come back equal so we never divide by 0.
    let cal1 = unsafe { core::ptr::read_volatile(TS_CAL1_ADDR) } as f32;
    let cal2 = unsafe { core::ptr::read_volatile(TS_CAL2_ADDR) } as f32;
    let slope = if (cal2 - cal1).abs() < 1.0 {
        0.0
    } else {
        (TS_CAL2_TEMP_C - TS_CAL1_TEMP_C) / (cal2 - cal1)
    };

    loop {
        Timer::after_secs(PERIOD_SECS).await;
        // 810.5 ADC cycles: the temp sensor is high-impedance and needs a long
        // sample window; timing is irrelevant at 0.2 Hz so take the max.
        let raw = adc.blocking_read(&mut ts, SampleTime::CYCLES810_5) as f32;
        let celsius = slope * (raw - cal1) + TS_CAL1_TEMP_C;
        // One decimal without float formatting: print whole + tenths separately.
        let tenths = (celsius * 10.0) as i32;
        crate::dbg_uart!("temp: {}.{} C (raw {})", tenths / 10, (tenths % 10).abs(), raw as u32);
    }
}
