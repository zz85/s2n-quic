// TODO: Remove when used
#![allow(dead_code)]

use alloc::collections::{
    btree_map::{Iter, Range},
    BTreeMap,
};
use s2n_quic_core::{
    packet::number::{PacketNumber, PacketNumberRange},
    time::Timestamp,
};

//= https://tools.ietf.org/id/draft-ietf-quic-recovery-29.txt#A.1

//= https://tools.ietf.org/id/draft-ietf-quic-recovery-29.txt#A.1.1
#[derive(Clone, Debug, Default)]
pub struct SentPackets {
    // TODO: Investigate a more efficient mechanism for managing sent_packets
    //       See https://github.com/awslabs/s2n-quic/issues/69
    sent_packets: BTreeMap<PacketNumber, SentPacketInfo>,
}

impl SentPackets {
    /// Inserts the given `sent_packet_info`
    pub fn insert(&mut self, packet_number: PacketNumber, sent_packet_info: SentPacketInfo) {
        self.sent_packets.insert(packet_number, sent_packet_info);
    }

    /// Returns a reference to the `SentPacketInfo` associated with the given `packet_number`
    pub fn get(&self, packet_number: PacketNumber) -> Option<&SentPacketInfo> {
        self.sent_packets.get(&packet_number)
    }

    /// Constructs a double-ended iterator over a sub-range of packet numbers
    pub fn range(&self, range: PacketNumberRange) -> Range<'_, PacketNumber, SentPacketInfo> {
        self.sent_packets.range(range.start()..=range.end())
    }

    /// Removes the `SentPacketInfo` associated with the given `packet_number`
    /// and returns the `SentPacketInfo` if it was present
    pub fn remove(&mut self, packet_number: PacketNumber) -> Option<SentPacketInfo> {
        self.sent_packets.remove(&packet_number)
    }

    /// Gets an iterator over the sent packet entries, sorted by PacketNumber
    pub fn iter(&self) -> Iter<'_, PacketNumber, SentPacketInfo> {
        self.sent_packets.iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SentPacketInfo {
    /// Indicates whether the packet counts towards bytes in flight
    pub in_flight: bool,
    /// The number of bytes sent in the packet, not including UDP or IP overhead,
    /// but including QUIC framing overhead
    pub sent_bytes: u64,
    /// The time the packet was sent
    pub time_sent: Timestamp,
}

impl SentPacketInfo {
    pub fn new(in_flight: bool, sent_bytes: u64, time_sent: Timestamp) -> Self {
        SentPacketInfo {
            in_flight,
            sent_bytes,
            time_sent,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::recovery::{SentPacketInfo, SentPackets};
    use s2n_quic_core::{
        packet::number::{PacketNumberRange, PacketNumberSpace},
        varint::VarInt,
    };

    #[test]
    fn insert_get_range() {
        let mut sent_packets = SentPackets::default();

        let packet_number_1 = PacketNumberSpace::Initial.new_packet_number(VarInt::from_u8(1));
        let sent_packet_1 = SentPacketInfo::new(false, 1, s2n_quic_platform::time::now());

        let packet_number_2 = PacketNumberSpace::Initial.new_packet_number(VarInt::from_u8(2));
        let sent_packet_2 = SentPacketInfo::new(false, 2, s2n_quic_platform::time::now());

        let packet_number_3 = PacketNumberSpace::Initial.new_packet_number(VarInt::from_u8(3));
        let sent_packet_3 = SentPacketInfo::new(false, 3, s2n_quic_platform::time::now());

        sent_packets.insert(packet_number_1, sent_packet_1);
        sent_packets.insert(packet_number_2, sent_packet_2);

        assert!(sent_packets.get(packet_number_1).is_some());
        assert!(sent_packets.get(packet_number_2).is_some());
        assert!(sent_packets.get(packet_number_3).is_none());

        assert_eq!(sent_packets.get(packet_number_1).unwrap(), &sent_packet_1);
        assert_eq!(sent_packets.get(packet_number_2).unwrap(), &sent_packet_2);

        sent_packets.insert(packet_number_3, sent_packet_3);

        assert!(sent_packets.get(packet_number_3).is_some());
        assert_eq!(sent_packets.get(packet_number_3).unwrap(), &sent_packet_3);

        for (&packet_number, &sent_packet_info) in
            sent_packets.range(PacketNumberRange::new(packet_number_1, packet_number_3))
        {
            assert_eq!(sent_packets.get(packet_number).unwrap(), &sent_packet_info);
        }

        for (&packet_number, &sent_packet_info) in sent_packets.iter() {
            assert_eq!(sent_packets.get(packet_number).unwrap(), &sent_packet_info);
        }
    }

    #[test]
    fn remove() {
        let mut sent_packets = SentPackets::default();
        let packet_number = PacketNumberSpace::Initial.new_packet_number(VarInt::from_u8(1));
        let sent_packet = SentPacketInfo::new(false, 0, s2n_quic_platform::time::now());
        sent_packets.insert(packet_number, sent_packet);

        assert!(sent_packets.get(packet_number).is_some());
        assert_eq!(sent_packets.get(packet_number).unwrap(), &sent_packet);

        assert_eq!(Some(sent_packet), sent_packets.remove(packet_number));

        assert!(sent_packets.get(packet_number).is_none());

        // Removing a packet that was already removed doesn't panic
        assert_eq!(None, sent_packets.remove(packet_number));
    }

    #[test]
    #[should_panic]
    fn wrong_packet_space_on_insert() {
        let mut sent_packets = new_sent_packets(PacketNumberSpace::Initial);

        let packet_number =
            PacketNumberSpace::ApplicationData.new_packet_number(VarInt::from_u8(1));
        let sent_packet = SentPacketInfo::new(false, 0, s2n_quic_platform::time::now());

        sent_packets.insert(packet_number, sent_packet);
    }

    #[test]
    #[should_panic]
    fn wrong_packet_space_on_get() {
        let sent_packets = new_sent_packets(PacketNumberSpace::Initial);

        let packet_number =
            PacketNumberSpace::ApplicationData.new_packet_number(VarInt::from_u8(1));
        sent_packets.get(packet_number);
    }

    #[test]
    #[should_panic]
    fn wrong_packet_space_on_range() {
        let sent_packets = new_sent_packets(PacketNumberSpace::Initial);

        let packet_number_start =
            PacketNumberSpace::ApplicationData.new_packet_number(VarInt::from_u8(1));
        let packet_number_end =
            PacketNumberSpace::ApplicationData.new_packet_number(VarInt::from_u8(2));
        sent_packets.range(PacketNumberRange::new(
            packet_number_start,
            packet_number_end,
        ));
    }

    #[test]
    #[should_panic]
    fn wrong_packet_space_on_remove() {
        let mut sent_packets = new_sent_packets(PacketNumberSpace::Initial);

        let packet_number =
            PacketNumberSpace::ApplicationData.new_packet_number(VarInt::from_u8(1));
        sent_packets.remove(packet_number);
    }

    fn new_sent_packets(space: PacketNumberSpace) -> SentPackets {
        let mut sent_packets = SentPackets::default();
        let packet_number = space.new_packet_number(VarInt::from_u8(1));
        let sent_packet = SentPacketInfo::new(false, 0, s2n_quic_platform::time::now());
        sent_packets.insert(packet_number, sent_packet);
        sent_packets
    }
}
