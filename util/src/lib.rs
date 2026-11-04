pub mod error;
pub mod crc;
pub mod io_utils;
pub mod binary;
pub use error::*;
pub use crc::*;
pub use io_utils::*;

pub type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

extern crate self as util;