use crate::counter::Counter;

/// environment variable for using PRR
#[cfg(feature = "std")]
const S2N_ENABLE_PRR_ENV: &str = "S2N_ENABLE_PRR";

/// Proportional Rate Reduction
/// https://www.rfc-editor.org/rfc/rfc6937.html
#[derive(Clone, Debug)]
pub struct Prr {
    /// Total bytes sent during recovery (prr_out)
    bytes_sent_during_recovery: usize,

    /// Total bytes delivered during recovery (prr_delivered)
    bytes_delivered_during_recovery: usize,

    /// FlightSize at the start of recovery (aka recoverfs)
    bytes_in_flight_at_recovery: usize,

    /// a local variable "sndcnt", which indicates exactly how
    /// many bytes should be sent in response to each ACK.
    bytes_allowed_on_ack: usize,
}

impl Prr {
    pub fn new() -> Self {
        Self {
            bytes_in_flight_at_recovery: 0,
            bytes_sent_during_recovery: 0,
            bytes_delivered_during_recovery: 0,
            bytes_allowed_on_ack: 0,
        }
    }

    pub fn on_congestion_event(&mut self, bytes_in_flight: Counter<u32>) {
        // on congestion window, reset all counters except for bytes_in_flight
        self.bytes_in_flight_at_recovery = *bytes_in_flight as usize;
        self.bytes_sent_during_recovery = 0;
        self.bytes_delivered_during_recovery = 0;
        self.bytes_allowed_on_ack = 0;
    }

    pub fn on_packet_sent(&mut self, bytes_sent: usize) {
        self.bytes_sent_during_recovery += bytes_sent;

        self.bytes_allowed_on_ack = self.bytes_allowed_on_ack.saturating_sub(bytes_sent);
    }

    pub fn on_ack(
        &mut self,
        bytes_acknowledged: usize,
        bytes_in_flight: Counter<u32>,
        slow_start_threshold: usize,
        max_datagram_size: u16,
    ) {
        let bytes_in_flight = *bytes_in_flight as usize;
        self.bytes_delivered_during_recovery += bytes_acknowledged;

        self.bytes_allowed_on_ack = if bytes_in_flight > slow_start_threshold {
            if self.bytes_in_flight_at_recovery == 0 {
                0
            } else {
                //= https://www.rfc-editor.org/rfc/rfc6937.html#section-3.1
                //# Proportional Rate Reduction
                //# sndcnt = CEIL(prr_delivered * ssthresh / RecoverFS) - prr_out
                ((
                    self.bytes_delivered_during_recovery * slow_start_threshold
                        + self.bytes_in_flight_at_recovery
                        - 1
                    // get around floating point conversions
                ) / self.bytes_in_flight_at_recovery)
                    .saturating_sub(self.bytes_delivered_during_recovery)
            }
        } else {
            // Slow Start Reduction Bound
            //= https://www.rfc-editor.org/rfc/rfc6937.html#section-3.1
            //# // PRR-SSRB
            //# limit = MAX(prr_delivered - prr_out, DeliveredData) + MSS
            let limit = self
                .bytes_delivered_during_recovery
                .saturating_sub(self.bytes_sent_during_recovery)
                .max(bytes_in_flight)
                + max_datagram_size as usize;

            //# Attempt to catch up, as permitted by limit
            limit.min(slow_start_threshold.saturating_sub(bytes_in_flight))
        };
    }

    pub fn can_transmit(&self, datagram_size: u16) -> bool {
        self.bytes_allowed_on_ack >= datagram_size as usize
    }

    #[cfg(feature = "std")]
    pub fn is_enabled(&self) -> bool {
        use once_cell::sync::OnceCell;
        static USE_PRR: OnceCell<bool> = OnceCell::new();
        *USE_PRR.get_or_init(|| std::env::var(S2N_ENABLE_PRR_ENV).is_ok())
    }

    #[cfg(not(feature = "std"))]
    pub fn is_enabled(&self) -> bool {
        false
    }
}
