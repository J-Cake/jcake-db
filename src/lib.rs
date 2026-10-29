#![feature(try_blocks)]

mod file;
// mod superblock;
mod error;
mod block;
mod page;
mod crc;

pub use error::Error;
pub use file::BlockDriver;
use crate::file::BlockDev;
use crate::crc::{crc, Crc};

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait StorageBackend {
    // fn initialise(self) -> Result<Self> where Self: Sized;

    fn index_db(&self) -> Result<(), Error>;
}

pub(crate) trait BinaryRepresentation<Block: BlockDev> {
    fn read_struct(backend: &mut Block) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: &mut Block) -> Result<()>;
}