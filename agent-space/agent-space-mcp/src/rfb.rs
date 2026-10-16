//! Client RFB parsing ported from the lab: websocket boundaries are not RFB
//! message boundaries. Frame requests and clipboard capability probes are passive.
use std::collections::HashMap;
#[derive(Default)]
pub(crate) struct Input {
    buffer: Vec<u8>,
    stage: u8,
    pressed: HashMap<(u8, u32), Vec<u8>>,
    pointer: Option<Vec<u8>>,
}
pub(crate) struct Packet {
    pub bytes: Vec<u8>,
    pub input: bool,
}
impl Input {
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Packet>, &'static str> {
        if self.buffer.len() + data.len() > 1024 * 1024 {
            return Err("RFB_BUFFER_LIMIT");
        }
        self.buffer.extend_from_slice(data);
        let mut result = vec![];
        while !self.buffer.is_empty() {
            let b = &self.buffer;
            let kind = b[0];
            let mut input = false;
            let n = if self.stage == 0 {
                12
            } else if self.stage < 3 {
                1
            } else {
                match kind {
                    0 => 20,
                    2 => {
                        if b.len() < 4 {
                            break;
                        }
                        4 + usize::from(u16::from_be_bytes([b[2], b[3]])) * 4
                    }
                    3 => 10,
                    4 => {
                        input = true;
                        8
                    }
                    5 => {
                        input = true;
                        6
                    }
                    6 => {
                        if b.len() < 8 {
                            break;
                        }
                        let size = i32::from_be_bytes(b[4..8].try_into().unwrap());
                        let size_abs = size.unsigned_abs() as usize;
                        if size_abs > 65536 {
                            return Err("RFB_CLIPBOARD_LIMIT");
                        }
                        if size < 0 {
                            if size_abs < 4 {
                                return Err("INVALID_CLIPBOARD");
                            }
                            if b.len() < 12 {
                                break;
                            }
                            let flags = u32::from_be_bytes(b[8..12].try_into().unwrap());
                            input = flags & 0x01000000 == 0 && flags & 0x18000000 != 0;
                        } else {
                            input = true;
                        }
                        8 + size_abs
                    }
                    150 => 10,
                    248 => {
                        if b.len() < 9 {
                            break;
                        }
                        9 + usize::from(b[8])
                    }
                    251 => {
                        if b.len() < 8 {
                            break;
                        }
                        input = true;
                        8 + usize::from(b[6]) * 16
                    }
                    255 => {
                        if b.len() < 2 {
                            break;
                        }
                        if b[1] != 0 {
                            return Err("UNSUPPORTED_KEY");
                        }
                        input = true;
                        12
                    }
                    _ => return Err("UNSUPPORTED_RFB_MESSAGE"),
                }
            };
            if b.len() < n {
                break;
            }
            let bytes = self.buffer.drain(..n).collect::<Vec<_>>();
            if self.stage == 0 && bytes.as_slice() != b"RFB 003.008\n" {
                return Err("UNSUPPORTED_RFB_VERSION");
            }
            if self.stage < 3 {
                self.stage += 1;
            }
            result.push(Packet { bytes, input });
        }
        Ok(result)
    }
    pub fn accepted(&mut self, packet: &[u8]) {
        let kind = packet[0];
        if kind == 4 || kind == 255 {
            let offset = if kind == 4 { 4 } else { 8 };
            let key = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
            let down = if kind == 4 {
                u16::from(packet[1])
            } else {
                u16::from_be_bytes([packet[2], packet[3]])
            };
            if down != 0 {
                let mut up = packet.to_vec();
                if kind == 4 {
                    up[1] = 0;
                } else {
                    up[2] = 0;
                    up[3] = 0;
                }
                self.pressed.insert((kind, key), up);
            } else {
                self.pressed.remove(&(kind, key));
            }
        }
        if kind == 5 {
            self.pointer = Some(packet.to_vec());
        }
    }
    pub fn release(&mut self) -> Vec<Vec<u8>> {
        let mut result = self
            .pressed
            .drain()
            .map(|(_, packet)| packet)
            .collect::<Vec<_>>();
        if let Some(mut pointer) = self.pointer.take() {
            pointer[1] = 0;
            result.push(pointer);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_packets_fail_before_unbounded_allocation() {
        let mut input = Input::default();
        assert_eq!(
            input.feed(&vec![0; 1024 * 1024 + 1]).err(),
            Some("RFB_BUFFER_LIMIT")
        );
        let mut input = Input::default();
        assert_eq!(
            input.feed(b"RFB 003.003\n").err(),
            Some("UNSUPPORTED_RFB_VERSION")
        );
        let mut input = Input::default();
        input.feed(b"RFB 003.008\n\x01\x01").unwrap();
        assert_eq!(
            input.feed(&[6, 0, 0, 0, 0, 1, 0, 1]).err(),
            Some("RFB_CLIPBOARD_LIMIT")
        );
    }
    #[test]
    fn clipboard_probes_are_passive_but_contents_require_control() {
        for (flags, expected) in [
            (0x01000001u32, false),
            (0x08000001, true),
            (0x10000001, true),
        ] {
            let mut input = Input::default();
            input.feed(b"RFB 003.008\n\x01\x01").unwrap();
            let mut data = vec![6, 0, 0, 0];
            data.extend((-4i32).to_be_bytes());
            data.extend(flags.to_be_bytes());
            assert_eq!(input.feed(&data).unwrap()[0].input, expected);
        }
    }
    #[test]
    fn fragmented_handshake_and_messages_are_reassembled() {
        let mut input = Input::default();
        assert!(input.feed(b"RFB 003.").unwrap().is_empty());
        assert_eq!(input.feed(b"008\n\x01\x01").unwrap().len(), 3);
        let mut bytes = vec![3, 0, 0, 0, 0, 0, 0, 1, 0, 1];
        bytes.extend([4, 1, 0, 0, 0, 0, 0, 65]);
        let packets = input.feed(&bytes).unwrap();
        assert!(!packets[0].input);
        assert!(packets[1].input);
        input.accepted(&packets[1].bytes);
        assert_eq!(input.release()[0][1], 0);
        assert!(input.release().is_empty());
    }
    #[test]
    fn clipboard_advertisement_is_passive() {
        let mut input = Input::default();
        input.feed(b"RFB 003.008\n\x01\x01").unwrap();
        let packet = [6, 0, 0, 0, 255, 255, 255, 252, 1, 0, 0, 0];
        assert!(!input.feed(&packet).unwrap()[0].input);
    }
}
