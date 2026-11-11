use std::borrow::Cow;
use std::fmt::Display;
use uuid::Bytes;
use crate::binary::Timestamp;

pub trait ToBytes {
	fn to_bytes(&self) -> Box<[u8]>;
}

/// Opt-in marker: "encode me via Display"
pub trait BytesViaDisplay: Display {}

impl<T: BytesViaDisplay> ToBytes for T {
	fn to_bytes(&self) -> Box<[u8]> {
		self.to_string().into_bytes().into_boxed_slice()
	}
}

// opt-ins, one line each (or generate them with a macro)
impl BytesViaDisplay for i64 {}
impl BytesViaDisplay for f64 {}
impl BytesViaDisplay for bool {}

// specialised impls; these don't conflict because Uuid isn't BytesViaDisplay
impl ToBytes for uuid::Uuid {
	fn to_bytes(&self) -> Box<[u8]> {
		Box::new(self.into_bytes()) // Box<[u8; 16]> coerces to Box<[u8]>
	}
}

impl ToBytes for String {
	fn to_bytes(&self) -> Box<[u8]> {
		self.as_bytes().into()
	}
}

impl ToBytes for str {
	fn to_bytes(&self) -> Box<[u8]> {
		self.as_bytes().into()
	}
}

impl ToBytes for &str {
	fn to_bytes(&self) -> Box<[u8]> {
		self.as_bytes().into()
	}
}

impl ToBytes for Timestamp {
	fn to_bytes(&self) -> Box<[u8]> {
		self.timestamp_micros().to_le_bytes().into()
	}
}

impl ToBytes for Box<[u8]> {
	fn to_bytes(&self) -> Box<[u8]> {
		self.as_ref().into()
	}
}