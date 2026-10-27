mod file;
mod superblock;
mod error;
mod block;

use std::io;

pub use error::Error;
pub use file::File;

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait StorageBackend {
    fn initialise(self) -> Result<Self> where Self: Sized;

    fn index_db(&self) -> Result<(), Error>;
}