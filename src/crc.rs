const POLY: u32 = 0x82F6_3B78; // CRC-32C, reflected

const TABLE: [u32; 256] = {
	let mut table = [0u32; 256];
	let mut i = 0;
	while i < 256 {
		let mut c = i as u32;
		let mut k = 0;
		while k < 8 {
			c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
			k += 1;
		}
		table[i] = c;
		i += 1;
	}
	table
};

/// Incremental CRC-32C, for checksumming several slices without concatenating them.
pub(crate) struct Crc(u32);

impl Crc {
	pub(crate) fn new() -> Self {
		Self(!0)
	}

	pub(crate) fn update(&mut self, bytes: impl AsRef<[u8]>) -> &mut Self {
		for &b in bytes.as_ref() {
			self.0 = TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
		}
		self
	}

	pub(crate) fn finish(&self) -> [u8; 4] {
		(!self.0).to_le_bytes()
	}
}

pub(crate) fn crc(bytes: impl AsRef<[u8]>) -> [u8; 4] {
	Crc::new().update(bytes).finish()
}

pub(crate) fn check_crc(bytes: impl AsRef<[u8]>, crc: u32) -> bool {
	Crc::new().update(bytes).finish() == crc.to_le_bytes()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn check_value() {
		// standard CRC-32C check value
		assert_eq!(crc(b"123456789"), 0xE306_9283u32.to_le_bytes());
	}
}