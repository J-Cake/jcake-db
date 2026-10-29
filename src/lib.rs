#![feature(try_blocks)]

mod file;
// mod superblock;
mod error;
mod page;
mod crc;
mod object;

use std::io;
pub use error::Error;
pub use file::BlockDriver;
use crate::file::BlockRead;
use crate::file::BlockWrite;
use crate::file::BlockSeek;
use crate::crc::{crc, Crc};

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait StorageBackend {
    // fn initialise(self) -> Result<Self> where Self: Sized;

    fn index_db(&self) -> Result<(), Error>;

}

pub(crate) trait BinaryRepresentation {
    fn read_struct(backend: impl BlockRead + BlockSeek) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: impl BlockWrite + BlockSeek) -> Result<()>;
}