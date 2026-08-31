use crate::xor::xor_u32;

/// Packed 32-bit DATUM header (OCEAN C bitfield, little-endian).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub cmd_len: u32,
    pub is_signed: bool,
    pub is_encrypted_pubkey: bool,
    pub is_encrypted_channel: bool,
    pub proto_cmd: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProtoCmd {
    HelloOrPing = 1,
    HandshakeResp = 2,
    Mining = 5,
    Info = 7,
}

impl Header {
    pub fn encode(self) -> [u8; 4] {
        let mut w = self.cmd_len & 0x3F_FFFF;
        if self.is_signed {
            w |= 1 << 24;
        }
        if self.is_encrypted_pubkey {
            w |= 1 << 25;
        }
        if self.is_encrypted_channel {
            w |= 1 << 26;
        }
        w |= (u32::from(self.proto_cmd) & 0x1F) << 27;
        w.to_le_bytes()
    }

    pub fn decode(raw: [u8; 4]) -> Self {
        let w = u32::from_le_bytes(raw);
        Self {
            cmd_len: w & 0x3F_FFFF,
            is_signed: (w >> 24) & 1 != 0,
            is_encrypted_pubkey: (w >> 25) & 1 != 0,
            is_encrypted_channel: (w >> 26) & 1 != 0,
            proto_cmd: ((w >> 27) & 0x1F) as u8,
        }
    }

    pub fn encode_obfuscated(self, key: u32) -> [u8; 4] {
        let mut b = self.encode();
        xor_u32(&mut b, key);
        b
    }

    pub fn decode_obfuscated(mut raw: [u8; 4], key: u32) -> Self {
        xor_u32(&mut raw, key);
        Self::decode(raw)
    }
}
