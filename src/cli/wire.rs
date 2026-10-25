//! The byte-level helpers the tool needs: hex, SHA-256, and the codec's
//! envelope encoding.
//!
//! The update envelope travels as protobuf, and the field numbers are the
//! schema's normative table
//! ([`tessellio-proto`](https://github.com/tessellio/tessellio-proto)). The
//! tool encodes and decodes that schema itself rather than taking a
//! dependency on a protobuf runtime: the message it carries has nine fields,
//! and the encoding it must produce is the schema's canonical one — a field
//! equal to its default is omitted, every other field is emitted.

use crate::cli::CliError;
use crate::format::{Codec, Region};

/// Parse a hexadecimal string into bytes.
///
/// Whitespace and `:` separators are ignored, so a byte string may be written
/// the way a packet capture writes it.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, CliError> {
    let mut nibbles: Vec<u8> = Vec::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_whitespace() || character == ':' {
            continue;
        }
        match character.to_digit(16) {
            Some(value) => nibbles.push(value as u8),
            None => {
                return Err(CliError::Input(format!(
                    "the hex text holds {character:?}, which is not a hexadecimal digit"
                )));
            }
        }
    }
    if nibbles.len() % 2 != 0 {
        return Err(CliError::Input(
            "the hex text holds an odd number of digits".to_string(),
        ));
    }
    Ok(nibbles
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect())
}

/// The SHA-256 of `bytes`, as 32 bytes.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = bytes.to_vec();
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0_u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

/// The SHA-256 of `bytes`, as 64 lowercase hexadecimal digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    crate::cli::hex(&sha256(bytes))
}

/// A protobuf writer: the schema's canonical encoding.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// An empty writer.
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// The bytes written so far.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn tag(&mut self, field: u32, wire_type: u32) {
        self.varint(u64::from((field << 3) | wire_type));
    }

    fn varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.bytes.push(byte);
                return;
            }
            self.bytes.push(byte | 0x80);
        }
    }

    /// Write an `int64` field, omitting it when it is the default.
    pub fn int64(&mut self, field: u32, value: i64) {
        if value == 0 {
            return;
        }
        self.tag(field, 0);
        self.varint(value as u64);
    }

    /// Write an `int32` field, omitting it when it is the default.
    pub fn int32(&mut self, field: u32, value: i32) {
        if value == 0 {
            return;
        }
        self.tag(field, 0);
        self.varint(value as i64 as u64);
    }

    /// Write an enum field, omitting it when it is the default.
    pub fn enum_value(&mut self, field: u32, value: i32) {
        self.int32(field, value);
    }

    /// Write a `bytes` field, omitting it when it is empty.
    pub fn bytes(&mut self, field: u32, value: &[u8]) {
        if value.is_empty() {
            return;
        }
        self.tag(field, 2);
        self.varint(value.len() as u64);
        self.bytes.extend_from_slice(value);
    }

    /// Write a length-delimited submessage, even when it is empty.
    pub fn message(&mut self, field: u32, value: &[u8]) {
        self.tag(field, 2);
        self.varint(value.len() as u64);
        self.bytes.extend_from_slice(value);
    }
}

/// A protobuf reader over one message.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// A reader over `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Whether every byte was consumed.
    pub fn is_empty(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    fn varint(&mut self) -> Result<u64, CliError> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| CliError::Input("the message ends inside a varint".to_string()))?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                return Err(CliError::Input(
                    "the message holds an overlong varint".to_string(),
                ));
            }
        }
    }

    fn fixed64(&mut self) -> Result<u64, CliError> {
        let end = self.offset + 8;
        if end > self.bytes.len() {
            return Err(CliError::Input(
                "the message ends inside a 64-bit field".to_string(),
            ));
        }
        let mut word = [0_u8; 8];
        word.copy_from_slice(&self.bytes[self.offset..end]);
        self.offset = end;
        Ok(u64::from_le_bytes(word))
    }

    fn length_delimited(&mut self) -> Result<&'a [u8], CliError> {
        let length = self.varint()? as usize;
        let end = self.offset + length;
        if end > self.bytes.len() {
            return Err(CliError::Input(
                "the message ends inside a length-delimited field".to_string(),
            ));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn skip(&mut self, wire_type: u32) -> Result<(), CliError> {
        match wire_type {
            0 => {
                self.varint()?;
            }
            1 => {
                self.fixed64()?;
            }
            2 => {
                self.length_delimited()?;
            }
            5 => {
                let end = self.offset + 4;
                if end > self.bytes.len() {
                    return Err(CliError::Input(
                        "the message ends inside a 32-bit field".to_string(),
                    ));
                }
                self.offset = end;
            }
            other => {
                return Err(CliError::Input(format!(
                    "the message holds the wire type {other}, which is not a protobuf wire type"
                )));
            }
        }
        Ok(())
    }
}

/// One part of one update, as it travels on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The update's id.
    pub update_id: u64,
    /// The update this one continues; `0` is a self-contained update.
    pub base_update_id: u64,
    /// The surface's width, in cells.
    pub width: u32,
    /// The surface's height, in cells.
    pub height: u32,
    /// This part's index inside the update.
    pub part_index: u32,
    /// The update's part count.
    pub part_count: u32,
    /// The sum of the update's region payload bytes.
    pub update_bytes: u64,
    /// The codec that produced the body.
    pub codec: i32,
    /// This part's regions.
    pub regions: Vec<Region>,
}

impl Envelope {
    /// The codec's name, when the envelope names one this build implements.
    pub fn codec_name(&self) -> &'static str {
        match Codec::from_wire_id(self.codec) {
            Some(codec) => codec.name(),
            None => "unknown",
        }
    }
}

/// Encode one part as the schema's `Update`.
pub fn encode_envelope(envelope: &Envelope) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.int64(1, envelope.update_id as i64);
    writer.int64(2, envelope.base_update_id as i64);
    writer.int32(3, envelope.width as i32);
    writer.int32(4, envelope.height as i32);
    writer.int32(5, envelope.part_index as i32);
    writer.int32(6, envelope.part_count as i32);
    writer.int64(7, envelope.update_bytes as i64);
    writer.enum_value(8, envelope.codec);
    for region in &envelope.regions {
        let mut region_writer = Writer::new();
        region_writer.int32(1, region.x as i32);
        region_writer.int32(2, region.y as i32);
        region_writer.int32(3, region.width as i32);
        region_writer.int32(4, region.height as i32);
        region_writer.bytes(5, &region.payload);
        writer.message(9, &region_writer.into_bytes());
    }
    writer.into_bytes()
}

/// Decode one part from the schema's `Update`.
///
/// A field the schema does not name is skipped, so a message that carries
/// more than this tool knows still decodes.
pub fn decode_envelope(bytes: &[u8]) -> Result<Envelope, CliError> {
    let mut reader = Reader::new(bytes);
    let mut envelope = Envelope {
        update_id: 0,
        base_update_id: 0,
        width: 0,
        height: 0,
        part_index: 0,
        part_count: 0,
        update_bytes: 0,
        codec: 0,
        regions: Vec::new(),
    };
    while !reader.is_empty() {
        let tag = reader.varint()?;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 7) as u32;
        match (field, wire_type) {
            (1, 0) => envelope.update_id = reader.varint()? as u64,
            (2, 0) => envelope.base_update_id = reader.varint()? as u64,
            (3, 0) => envelope.width = reader.varint()? as u32,
            (4, 0) => envelope.height = reader.varint()? as u32,
            (5, 0) => envelope.part_index = reader.varint()? as u32,
            (6, 0) => envelope.part_count = reader.varint()? as u32,
            (7, 0) => envelope.update_bytes = reader.varint()? as u64,
            (8, 0) => envelope.codec = reader.varint()? as i32,
            (9, 2) => {
                let bytes = reader.length_delimited()?;
                envelope.regions.push(decode_region(bytes)?);
            }
            _ => reader.skip(wire_type)?,
        }
    }
    Ok(envelope)
}

/// Decode one region from the schema's `Region`.
fn decode_region(bytes: &[u8]) -> Result<Region, CliError> {
    let mut reader = Reader::new(bytes);
    let mut region = Region {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        payload: Vec::new(),
    };
    while !reader.is_empty() {
        let tag = reader.varint()?;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 7) as u32;
        match (field, wire_type) {
            (1, 0) => region.x = reader.varint()? as u32,
            (2, 0) => region.y = reader.varint()? as u32,
            (3, 0) => region.width = reader.varint()? as u32,
            (4, 0) => region.height = reader.varint()? as u32,
            (5, 2) => region.payload = reader.length_delimited()?.to_vec(),
            _ => reader.skip(wire_type)?,
        }
    }
    Ok(region)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_text_is_read_with_separators_and_whitespace() {
        assert_eq!(
            parse_hex("4d 4c:50 00").expect("hex"),
            vec![0x4d, 0x4c, 0x50, 0x00]
        );
    }

    #[test]
    fn an_odd_hex_text_is_refused() {
        let error = parse_hex("abc").expect_err("odd");
        assert_eq!(error.exit_code(), crate::cli::EXIT_INPUT);
    }

    #[test]
    fn an_envelope_round_trips() {
        let envelope = Envelope {
            update_id: 7,
            base_update_id: 1,
            width: 8,
            height: 4,
            part_index: 0,
            part_count: 1,
            update_bytes: 22,
            codec: 8,
            regions: vec![Region {
                x: 2,
                y: 1,
                width: 4,
                height: 2,
                payload: vec![1, 2, 3, 4],
            }],
        };
        let bytes = encode_envelope(&envelope);
        assert_eq!(decode_envelope(&bytes).expect("decodes"), envelope);
    }

    #[test]
    fn a_default_envelope_is_an_empty_message() {
        let envelope = Envelope {
            update_id: 0,
            base_update_id: 0,
            width: 0,
            height: 0,
            part_index: 0,
            part_count: 0,
            update_bytes: 0,
            codec: 0,
            regions: Vec::new(),
        };
        assert!(encode_envelope(&envelope).is_empty());
    }

    #[test]
    fn the_encoding_is_the_schemas_canonical_one() {
        // The hand-derived vector V1 of the conformance corpus: an idle
        // update whose header is `08 07 10 06 18 08 20 04 30 01`.
        let envelope = Envelope {
            update_id: 7,
            base_update_id: 6,
            width: 8,
            height: 4,
            part_index: 0,
            part_count: 1,
            update_bytes: 0,
            codec: 8,
            regions: Vec::new(),
        };
        assert_eq!(
            crate::cli::hex(&encode_envelope(&envelope)),
            "080710061808200430014008"
        );
    }

    #[test]
    fn an_unknown_field_is_skipped() {
        let mut writer = Writer::new();
        writer.int64(1, 4);
        writer.int32(99, 7);
        writer.int64(2, 1);
        let envelope = decode_envelope(&writer.into_bytes()).expect("decodes");
        assert_eq!(envelope.update_id, 4);
        assert_eq!(envelope.base_update_id, 1);
    }

    #[test]
    fn a_truncated_message_is_refused() {
        let mut writer = Writer::new();
        writer.int64(1, 4);
        let mut bytes = writer.into_bytes();
        bytes.push(0x08);
        let error = decode_envelope(&bytes).expect_err("truncated");
        assert_eq!(error.exit_code(), crate::cli::EXIT_INPUT);
    }
}
