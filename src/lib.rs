#![feature(try_blocks)]

mod file;
// mod superblock;
mod error;
mod page;
mod crc;
mod object;

use std::io;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use uuid::Uuid;
pub use error::Error;
pub use file::BlockDriver;
use crate::file::BlockRead;
use crate::file::BlockWrite;
use crate::file::BlockSeek;
use crate::crc::{crc, Crc};

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait ObjectStore {
    fn index_db(&self) -> Result<()>;
    fn compact_db(&mut self) -> Result<()>;

    fn append_object(&mut self, obj: &[u8]) -> Result<Uuid>;
    fn search_object(&mut self, obj: Uuid) -> Option<Box<[u8]>>;
}

pub struct TypedDatabase(Box<dyn ObjectStore>);

impl TypedDatabase {
    fn append_object(&mut self, obj: impl Serialize) -> Result<Uuid> {
        let obj = jsonb::to_owned_jsonb(&obj)?;
        self.0.append_object(&obj.as_ref())
    }

    fn search_object<T: DeserializeOwned>(&mut self, obj: impl AsRef<Uuid>) -> Option<Result<T>> {
        let obj = self.0.search_object(obj.as_ref().clone())?;
        let raw = jsonb::RawJsonb::new(&obj);

        Some(jsonb::from_raw_jsonb(&raw)
            .map_err(Into::into))
    }
}

pub(crate) trait BinaryRepresentation {
    fn read_struct(backend: impl BlockRead + BlockSeek) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: impl BlockWrite + BlockSeek) -> Result<()>;
}