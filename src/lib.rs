#![feature(try_blocks)]
#![feature(map_try_insert)]

mod file;
// mod superblock;
mod error;
mod page;
mod crc;
mod object;
mod io_utils;

use std::fs;
use std::path::Path;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
pub use error::Error;
pub use file::BlockDriver;
use crate::crc::crc;
use crate::file::{blank, open};
use crate::io_utils::{Device, Reader, Writer};
use crate::object::Object;

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait ObjectStore {
    fn compact_db(&mut self) -> Result<()>;

    fn search(&self, id: Uuid) -> Result<Option<Object>>;
    fn insert(&mut self, data: &[u8]) -> Result<Uuid>;
    fn update(&mut self, id: Uuid, data: &[u8]) -> Result<()>;
    fn delete(&mut self, id: Uuid) -> Result<()>;
}

pub struct TypedDatabase<Db: ObjectStore>(Db);

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub fn insert(&mut self, data: impl Serialize) -> Result<Uuid> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.0.insert(&obj.as_ref())
    }

    pub fn search<T: DeserializeOwned>(&self, id: Uuid) -> Result<Option<T>> {
        let Some(obj) = self.0.search(id)? else {
            return Ok(None);
        };
        let raw = jsonb::RawJsonb::new(&obj);
        let obj = jsonb::from_raw_jsonb(&raw)
            .map_err(Into::<Error>::into)?;

        Ok(Some(obj))
    }
}

impl TypedDatabase<BlockDriver<fs::File>> {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            Ok(Self(open(path)?))
        } else {
            Ok(Self(blank(path)?))
        }
    }
}

pub(crate) trait BinaryRepresentation<Dev: Device> {
    fn read_struct(mut backend: Reader<Dev>, pos: u64) -> Result<Self> where Self: Sized {
        backend.seek_to(pos);
        Self::read_at_position(backend)
    }
    fn read_at_position(backend: Reader<Dev>) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: &mut Writer<Dev>) -> Result<()>;
}