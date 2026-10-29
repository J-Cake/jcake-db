#![feature(try_blocks)]

mod file;
// mod superblock;
mod error;
mod page;
mod crc;
mod object;
mod io_utils;

use std::path::Path;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
pub use error::Error;
pub use file::BlockDriver;
use crate::crc::crc;
use crate::io_utils::Device;
use crate::object::Object;

pub(crate) type Result<T, Error = crate::Error> = std::result::Result<T, Error>;

pub trait ObjectStore {
    fn compact_db(&mut self) -> Result<()>;

    fn search(&self, id: Uuid) -> Result<Option<Object>>;
    fn insert(&mut self, data: &[u8]) -> Result<Uuid>;
    fn update(&mut self, id: Uuid, data: &[u8]) -> Result<()>;
    fn delete(&mut self, id: Uuid) -> Result<()>;
}

pub struct TypedDatabase(Box<dyn ObjectStore>);

impl TypedDatabase {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            Ok(Self(Box::new(BlockDriver::open(path)?)))
        } else {
            Ok(Self(Box::new(BlockDriver::blank(path)?)))
        }
    }

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

pub(crate) trait BinaryRepresentation<Dev: Device> {
    fn read_struct(backend: &Dev) -> Result<Self> where Self: Sized;
    fn write_struct(&self, backend: &mut Dev) -> Result<()>;
}