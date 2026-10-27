mod file;
mod superblock;
mod error;
mod block;
mod page;

use std::io;
use std::io::Bytes;
pub use error::Error;
pub use file::File;

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait StorageBackend {
    fn initialise(self) -> Result<Self> where Self: Sized;

    fn index_db(&self) -> Result<(), Error>;

    fn read(&mut self, len: usize) -> Result<Box<[u8]>>;
    fn write(&mut self, buffer: impl AsRef<[u8]>) -> Result<()>;
    fn seek(&mut self, by: isize) -> Result<()>;
}

pub(crate) trait BinaryRepresentation<Backend: StorageBackend> {
    fn read_struct(backend: &mut Backend) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: &mut Backend) -> Result<()>;
}