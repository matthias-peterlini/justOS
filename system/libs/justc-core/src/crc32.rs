//! CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320).

const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &b| TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8))
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_vector() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
    }
}
