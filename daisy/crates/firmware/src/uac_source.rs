//! USB Audio Class 1.0 **source** (capture) device — the Daisy presents to the
//! Pi as a microphone/line-in, streaming our audio out over an isochronous IN
//! endpoint for the visualizer's `getUserMedia`.
//!
//! Ported from embassy `main`'s `embassy-usb/src/class/uac1/source.rs` onto our
//! pinned embassy-usb 0.6: the public `Builder`/`InterfaceAltBuilder` APIs match
//! upstream, so only the imports change and the otherwise-private `class_codes`
//! / `terminal_type` constants are redefined locally here (they're plain UAC1
//! spec values).

use core::marker::PhantomData;

use defmt::{debug, error, info};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pubsub::{PubSubChannel, Publisher, Subscriber};
use embassy_usb::class::uac1::SampleWidth;
use embassy_usb::control::{InResponse, OutResponse, Recipient, Request, RequestType};
use embassy_usb::descriptor::{SynchronizationType, UsageType};
use embassy_usb::driver::{Driver, Endpoint, EndpointAddress, EndpointError, EndpointIn, EndpointType};
use embassy_usb::types::InterfaceNumber;
use embassy_usb::{Builder, Handler, InterfaceAltBuilder};
use heapless::Vec;

// --- UAC1 spec constants (mirror embassy's private uac1::class_codes) --------
const USB_AUDIO_CLASS: u8 = 0x01;
const USB_AUDIOCONTROL_SUBCLASS: u8 = 0x01;
const USB_AUDIOSTREAMING_SUBCLASS: u8 = 0x02;
const PROTOCOL_NONE: u8 = 0x00;

const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;

const HEADER_SUBTYPE: u8 = 0x01;
const INPUT_TERMINAL: u8 = 0x02;
const OUTPUT_TERMINAL: u8 = 0x03;
const FEATURE_UNIT: u8 = 0x06;

const AS_GENERAL: u8 = 0x01;
const FORMAT_TYPE: u8 = 0x02;
const FORMAT_TYPE_I: u8 = 0x01;
const EP_GENERAL: u8 = 0x01;

const PCM: u16 = 0x0001;
const ADC_VERSION: u16 = 0x0100;

const MUTE_CONTROL: u8 = 0x01;
const VOLUME_CONTROL: u8 = 0x02;

const SET_CUR: u8 = 0x01;
const GET_CUR: u8 = 0x81;
const SET_RES: u8 = 0x04;
const GET_MIN: u8 = 0x82;
const GET_MAX: u8 = 0x83;
const GET_RES: u8 = 0x84;

// Terminal types (mirror embassy's private uac1::terminal_type::TerminalType).
const TT_USB_STREAMING: u16 = 0x0101;
const TT_IN_MICROPHONE: u16 = 0x0201;
// wChannelConfig: bit0 = left front, bit1 = right front.
const CHANNEL_CONFIG_STEREO: u16 = 0x0001 | 0x0002;

// --- Unit IDs ----------------------------------------------------------------
const INPUT_UNIT_ID: u8 = 0x01;
const FEATURE_UNIT_ID: u8 = 0x02;
const OUTPUT_UNIT_ID: u8 = 0x03;

const MAX_AUDIO_CHANNEL_COUNT: usize = 0x02;

// --- Sample-rate sharing channel ---------------------------------------------
const SR_CH_CAP: usize = 4;
const SR_CH_SUBS: usize = 4;
const SR_CH_PUBS: usize = 4;

type SampleRateChannel = PubSubChannel<CriticalSectionRawMutex, u32, SR_CH_CAP, SR_CH_SUBS, SR_CH_PUBS>;
type SampleRatePub = Publisher<'static, CriticalSectionRawMutex, u32, SR_CH_CAP, SR_CH_PUBS, SR_CH_PUBS>;
type SampleRateSub = Subscriber<'static, CriticalSectionRawMutex, u32, SR_CH_CAP, SR_CH_SUBS, SR_CH_PUBS>;

static SAMPLE_RATE_CHANNEL: SampleRateChannel = SampleRateChannel::new();

fn sample_rate_publisher() -> SampleRatePub {
    SAMPLE_RATE_CHANNEL.publisher().unwrap()
}

/// Subscribe to host-requested sample-rate changes.
pub fn sample_rate_subscriber() -> SampleRateSub {
    SAMPLE_RATE_CHANNEL.subscriber().unwrap()
}

fn calculate_max_packet_size(sample_rate_hz: u32, num_channels: u8, b_subframe_size: u8) -> u16 {
    let bytes_per_ms = (sample_rate_hz * num_channels as u32 * b_subframe_size as u32) / 1000;
    debug!(
        "uac: max_packet_size: {}Hz x {}ch x {}B = {} B/ms",
        sample_rate_hz, num_channels, b_subframe_size, bytes_per_ms
    );
    bytes_per_ms as u16
}

/// Isochronous IN endpoint (audio stream -> host, or feedback -> host).
pub struct AudioSourceEpIn<'d, D: Driver<'d>> {
    ep: D::EndpointIn,
}

impl<'d, D: Driver<'d>> AudioSourceEpIn<'d, D> {
    /// Write one packet to the endpoint.
    pub async fn write(&mut self, buf: &[u8]) -> Result<(), EndpointError> {
        self.ep.write(buf).await
    }

    /// Write `buf` one wMaxPacketSize chunk at a time.
    pub async fn write_as_chunks(&mut self, buf: &[u8], needs_zlp: bool) -> Result<(), EndpointError> {
        self.ep.write_transfer(buf, needs_zlp).await
    }

    /// Wait until the host activates the streaming alt-setting (otherwise writes
    /// fail with `EndpointError::Disabled`).
    pub async fn wait_enabled(&mut self) {
        self.ep.wait_enabled().await
    }
}

/// The Audio Source class: builds the descriptors + allocates the endpoints.
pub struct AudioSource<'d, D: Driver<'d>> {
    phantom: PhantomData<&'d D>,
}

impl<'d, D: Driver<'d>> AudioSource<'d, D> {
    fn create_control_function(b: &mut InterfaceAltBuilder<'_, 'd, D>, streaming_interface: u8) {
        // 4.3.2.1 Input Terminal Descriptor (the audio source = "microphone").
        let w_terminal_type: u16 = TT_IN_MICROPHONE;
        let channels_cfg: u16 = CHANNEL_CONFIG_STEREO;
        let input_terminal_descriptor: [u8; 10] = [
            INPUT_TERMINAL,
            INPUT_UNIT_ID,
            w_terminal_type as u8,
            (w_terminal_type >> 8) as u8,
            0x00, // bAssocTerminal
            MAX_AUDIO_CHANNEL_COUNT as u8,
            channels_cfg as u8,
            (channels_cfg >> 8) as u8,
            0x00, // iChannelNames
            0x00, // iTerminal
        ];

        // 4.3.2.5 Feature Unit Descriptor (master mute + volume).
        let controls: u8 = MUTE_CONTROL | VOLUME_CONTROL;
        const FEATURE_UNIT_DESCRIPTOR_SIZE: usize = 5;
        let mut feature_unit_descriptor: Vec<u8, { FEATURE_UNIT_DESCRIPTOR_SIZE + MAX_AUDIO_CHANNEL_COUNT + 1 }> =
            Vec::from_slice(&[FEATURE_UNIT, FEATURE_UNIT_ID, INPUT_UNIT_ID, 1, controls]).unwrap();
        for _ in 0..MAX_AUDIO_CHANNEL_COUNT {
            feature_unit_descriptor.push(controls).unwrap();
        }
        feature_unit_descriptor.push(0x00).unwrap(); // iFeature

        // 4.3.2.2 Output Terminal Descriptor (USB streaming, fed by feature unit).
        let terminal_type: u16 = TT_USB_STREAMING;
        let output_terminal_descriptor = [
            OUTPUT_TERMINAL,
            OUTPUT_UNIT_ID,
            terminal_type as u8,
            (terminal_type >> 8) as u8,
            0x00,            // bAssocTerminal
            FEATURE_UNIT_ID, // bSourceID
            0x00,            // iTerminal
        ];

        // 4.3.2 Class-Specific AC Interface (Header) Descriptor.
        const AC_HEADER_SIZE: usize = 2;
        const INTERFACE_DESCRIPTOR_SIZE: usize = 7;
        let mut total_descriptor_length: usize = 0;
        for size in [
            INTERFACE_DESCRIPTOR_SIZE,
            input_terminal_descriptor.len(),
            feature_unit_descriptor.len(),
            output_terminal_descriptor.len(),
        ] {
            total_descriptor_length += size + AC_HEADER_SIZE;
        }
        let interface_descriptor: [u8; INTERFACE_DESCRIPTOR_SIZE] = [
            HEADER_SUBTYPE,
            ADC_VERSION as u8,
            (ADC_VERSION >> 8) as u8,
            total_descriptor_length as u8,
            (total_descriptor_length >> 8) as u8,
            0x01,                // bInCollection
            streaming_interface, // baInterfaceNr
        ];

        b.descriptor(CS_INTERFACE, &interface_descriptor);
        b.descriptor(CS_INTERFACE, &input_terminal_descriptor);
        b.descriptor(CS_INTERFACE, &feature_unit_descriptor);
        b.descriptor(CS_INTERFACE, &output_terminal_descriptor);
    }

    fn create_streaming_iface_active(
        b: &mut InterfaceAltBuilder<'_, 'd, D>,
        sample_rates: &[u32],
        sample_width: SampleWidth,
        feedback_refresh_period_ms: u8,
    ) -> (D::EndpointIn, D::EndpointIn) {
        // Class-specific AS general descriptor.
        b.descriptor(
            CS_INTERFACE,
            &[
                AS_GENERAL,
                OUTPUT_UNIT_ID, // bTerminalLink
                0x01,           // bDelay
                PCM as u8,
                (PCM >> 8) as u8,
            ],
        );

        let min_rate = sample_rates.iter().min().unwrap();
        let max_rate = sample_rates.iter().max().unwrap();

        // Format Type I descriptor (continuous min..max range).
        let format_type_i_body: [u8; 12] = [
            FORMAT_TYPE,
            FORMAT_TYPE_I,
            MAX_AUDIO_CHANNEL_COUNT as u8,
            sample_width as u8,
            sample_width.in_bit() as u8,
            0x00, // bSamFreqType: continuous range
            (min_rate & 0xFF) as u8,
            ((min_rate >> 8) & 0xFF) as u8,
            ((min_rate >> 16) & 0xFF) as u8,
            (max_rate & 0xFF) as u8,
            ((max_rate >> 8) & 0xFF) as u8,
            ((max_rate >> 16) & 0xFF) as u8,
        ];
        b.descriptor(CS_INTERFACE, &format_type_i_body);

        // Isochronous IN endpoint for audio data (device -> host).
        let max_packet_size: u16 =
            calculate_max_packet_size(*max_rate, MAX_AUDIO_CHANNEL_COUNT as u8, sample_width as u8);
        let audio_in_endpoint = b.alloc_endpoint_in(EndpointType::Isochronous, None, max_packet_size, 1);
        debug!(
            "uac: audio EP addr={:?} mps={} interval={}",
            audio_in_endpoint.info().addr,
            audio_in_endpoint.info().max_packet_size,
            audio_in_endpoint.info().interval_ms,
        );

        // Optional isochronous IN feedback endpoint.
        let feedback_in_endpoint =
            b.alloc_endpoint_in(EndpointType::Isochronous, None, 4, feedback_refresh_period_ms);
        debug!(
            "uac: feedback EP addr={:?} interval={}",
            feedback_in_endpoint.info().addr,
            feedback_in_endpoint.info().interval_ms,
        );

        // Standard endpoint descriptor for the audio IN endpoint (links feedback).
        b.endpoint_descriptor(
            audio_in_endpoint.info(),
            SynchronizationType::Asynchronous,
            UsageType::DataEndpoint,
            &[
                feedback_refresh_period_ms,
                feedback_in_endpoint.info().addr.into(),
            ],
        );

        // Class-specific endpoint descriptor for the audio endpoint.
        b.descriptor(
            CS_ENDPOINT,
            &[
                EP_GENERAL, 0x01, // bmAttributes: sampling frequency control
                0x02, // bLockDelayUnits: PCM samples
                0x00, 0x00, // wLockDelay
            ],
        );

        // Standard endpoint descriptor for the feedback IN endpoint.
        b.endpoint_descriptor(
            feedback_in_endpoint.info(),
            SynchronizationType::NoSynchronization,
            UsageType::FeedbackEndpoint,
            &[],
        );

        (audio_in_endpoint, feedback_in_endpoint)
    }

    /// Build the audio-source function (control IF + streaming IF) and return
    /// (audio EP, feedback EP, control handler).
    pub fn new(
        b: &mut Builder<'d, D>,
        sample_rates: &'static [u32],
        sample_width: SampleWidth,
        feedback_refresh_period_ms: u8,
    ) -> (AudioSourceEpIn<'d, D>, AudioSourceEpIn<'d, D>, AudioSourceControlHandler) {
        let mut func = b.function(USB_AUDIO_CLASS, USB_AUDIOCONTROL_SUBCLASS, PROTOCOL_NONE);

        // Audio Control interface (IF 0), single alt setting.
        let mut iface_ctrl = func.interface();
        let iface_ctrl_num = iface_ctrl.interface_number();
        let ba_iface_nr = u8::from(iface_ctrl_num) + 1;
        {
            let mut alt_ctrl = iface_ctrl.alt_setting(USB_AUDIO_CLASS, USB_AUDIOCONTROL_SUBCLASS, PROTOCOL_NONE, None);
            Self::create_control_function(&mut alt_ctrl, ba_iface_nr);
        }

        // Audio Streaming interface (IF 1): alt 0 = zero-bandwidth, alt 1 = active.
        let mut iface_stream = func.interface();
        let iface_stream_num = iface_stream.interface_number();
        let alt0 = iface_stream.alt_setting(USB_AUDIO_CLASS, USB_AUDIOSTREAMING_SUBCLASS, PROTOCOL_NONE, None);
        drop(alt0);
        let mut alt1 = iface_stream.alt_setting(USB_AUDIO_CLASS, USB_AUDIOSTREAMING_SUBCLASS, PROTOCOL_NONE, None);
        let (ep_audio_in, ep_feedback_in) =
            Self::create_streaming_iface_active(&mut alt1, sample_rates, sample_width, feedback_refresh_period_ms);

        let ep_audio_addr = ep_audio_in.info().addr;
        let ep_feedback_addr = ep_feedback_in.info().addr;

        (
            AudioSourceEpIn { ep: ep_audio_in },
            AudioSourceEpIn { ep: ep_feedback_in },
            AudioSourceControlHandler::new(
                sample_rates,
                ep_audio_addr,
                ep_feedback_addr,
                iface_ctrl_num,
                iface_stream_num,
            ),
        )
    }
}

/// Handles class-specific control requests (volume, mute, sample rate).
pub struct AudioSourceControlHandler {
    current_volume: [i16; 3],
    current_mute: [u8; 3],
    current_sample_rate_index: usize,
    supported_sample_rates: &'static [u32],
    sample_rate_ch_pub: SampleRatePub,
    ep_audio_addr: EndpointAddress,
    ep_feedback_addr: EndpointAddress,
    iface_ctrl_num: InterfaceNumber,
    iface_stream_num: InterfaceNumber,
}

impl AudioSourceControlHandler {
    pub fn new(
        sample_rates: &'static [u32],
        ep_audio_addr: EndpointAddress,
        ep_feedback_addr: EndpointAddress,
        iface_ctrl_num: InterfaceNumber,
        iface_stream_num: InterfaceNumber,
    ) -> Self {
        AudioSourceControlHandler {
            current_volume: [0, 0, 0],
            current_mute: [0, 0, 0],
            current_sample_rate_index: 0,
            supported_sample_rates: sample_rates,
            sample_rate_ch_pub: sample_rate_publisher(),
            ep_audio_addr,
            ep_feedback_addr,
            iface_ctrl_num,
            iface_stream_num,
        }
    }

    fn handle_control_in<'r>(&'r mut self, req: Request, data: &'r mut [u8]) -> Option<InResponse<'r>> {
        if req.request_type != RequestType::Class || req.recipient != Recipient::Interface {
            return Some(InResponse::Rejected);
        }
        if (req.index & 0xFF) as u8 != 0 {
            return Some(InResponse::Rejected);
        }
        let control_selector = (req.value >> 8) as u8;
        let channel = (req.value & 0xFF) as usize;
        match req.request {
            GET_CUR => {
                if control_selector == VOLUME_CONTROL && channel < 3 {
                    data[0..2].copy_from_slice(&self.current_volume[channel].to_le_bytes());
                    return Some(InResponse::Accepted(&data[0..2]));
                } else if control_selector == MUTE_CONTROL && channel < 3 {
                    data[0] = self.current_mute[channel];
                    return Some(InResponse::Accepted(&data[0..1]));
                }
            }
            GET_MIN | GET_MAX | GET_RES => {
                if control_selector == VOLUME_CONTROL && channel < 3 {
                    let value = match req.request {
                        GET_MIN => -12750i16,
                        GET_MAX => 0i16,
                        GET_RES => 256i16,
                        _ => unreachable!(),
                    };
                    data[0..2].copy_from_slice(&value.to_le_bytes());
                    return Some(InResponse::Accepted(&data[0..2]));
                }
            }
            _ => {}
        }
        Some(InResponse::Rejected)
    }

    fn handle_control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if req.request_type != RequestType::Class || req.recipient != Recipient::Interface {
            return Some(OutResponse::Rejected);
        }
        if (req.index & 0xFF) as u8 != 0 {
            return Some(OutResponse::Rejected);
        }
        let control_selector = (req.value >> 8) as u8;
        let channel = (req.value & 0xFF) as usize;
        match req.request {
            SET_CUR | SET_RES => match control_selector {
                VOLUME_CONTROL if channel < 3 && data.len() >= 2 => {
                    self.current_volume[channel] = i16::from_le_bytes([data[0], data[1]]);
                    Some(OutResponse::Accepted)
                }
                MUTE_CONTROL if channel < 3 && !data.is_empty() => {
                    self.current_mute[channel] = data[0];
                    Some(OutResponse::Accepted)
                }
                _ => Some(OutResponse::Rejected),
            },
            _ => Some(OutResponse::Rejected),
        }
    }

    fn handle_ep_in<'a>(&mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if req.request_type != RequestType::Class {
            return Some(InResponse::Rejected);
        }
        if req.request == GET_CUR && req.index & 0xFF == u8::from(self.ep_audio_addr) as u16 {
            let r = self.supported_sample_rates[self.current_sample_rate_index];
            buf[0] = (r & 0xFF) as u8;
            buf[1] = ((r >> 8) & 0xFF) as u8;
            buf[2] = ((r >> 16) & 0xFF) as u8;
            return Some(InResponse::Accepted(&buf[0..3]));
        }
        Some(InResponse::Rejected)
    }

    fn handle_ep_out(&mut self, req: Request, buf: &[u8]) -> Option<OutResponse> {
        if req.request_type != RequestType::Class {
            return Some(OutResponse::Rejected);
        }
        if req.request == SET_CUR && req.index & 0xFF == u8::from(self.ep_audio_addr) as u16 && buf.len() >= 3 {
            let rate = (buf[0] as u32) | ((buf[1] as u32) << 8) | ((buf[2] as u32) << 16);
            if let Ok(index) = self.supported_sample_rates.binary_search(&rate) {
                self.current_sample_rate_index = index;
            }
            if self.sample_rate_ch_pub.try_publish(rate).is_err() {
                error!("uac: failed to publish sample rate");
            }
            return Some(OutResponse::Accepted);
        }
        Some(OutResponse::Rejected)
    }

    /// bInterfaceNumber of the control interface.
    pub fn ctrl_iface_num(&self) -> u8 {
        u8::from(self.iface_ctrl_num)
    }

    /// bInterfaceNumber of the streaming interface.
    pub fn stream_iface_num(&self) -> u8 {
        u8::from(self.iface_stream_num)
    }

    /// Address of the feedback endpoint.
    pub fn feedback_ep_addr(&self) -> u8 {
        u8::from(self.ep_feedback_addr)
    }
}

impl Handler for AudioSourceControlHandler {
    fn configured(&mut self, configured: bool) {
        info!("uac: configured={}", configured);
    }

    fn set_alternate_setting(&mut self, iface: InterfaceNumber, alternate_setting: u8) {
        if iface == self.iface_stream_num {
            info!("uac: streaming alt-setting -> {}", alternate_setting);
        }
    }

    fn control_out(&mut self, req: Request, buf: &[u8]) -> Option<OutResponse> {
        match (req.request_type, req.recipient) {
            (RequestType::Class, Recipient::Interface) => self.handle_control_out(req, buf),
            (RequestType::Class, Recipient::Endpoint) => self.handle_ep_out(req, buf),
            _ => None,
        }
    }

    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        match (req.request_type, req.recipient) {
            (RequestType::Class, Recipient::Interface) => self.handle_control_in(req, buf),
            (RequestType::Class, Recipient::Endpoint) => self.handle_ep_in(req, buf),
            _ => None,
        }
    }
}
