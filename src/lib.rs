#![feature(map_try_insert)]
extern crate core;

mod file;
mod page;
mod object;

use std::fs;
use std::path::Path;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use uuid::Uuid;
pub use file::BlockDriver;
pub use util::Device;
pub use util::Error;
pub use util::Reader;
pub use util::Writer;
pub use util::Result;
use crate::file::blank;
use crate::file::open;
use crate::object::Object;

pub trait ObjectStore: Sized {
    fn compact_db(&mut self) -> Result<()>;
    fn entries(&self) -> impl Iterator<Item=Object>;

    fn search(&self, id: Uuid) -> Result<Option<Object>>;
    fn insert(&mut self, data: &[u8]) -> Result<Uuid>;
    fn upsert(&mut self, id: Uuid, data: &[u8]) -> Result<()>;
    fn delete(&mut self, id: Uuid) -> Result<()>;
}

pub struct TypedDatabase<Db: ObjectStore>(Db);

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub fn insert(&mut self, data: impl Serialize) -> Result<Uuid> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.0.insert(&obj.as_ref())
    }

    pub fn upsert(&mut self, uuid: Uuid, data: impl Serialize) -> Result<()> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.0.upsert(uuid, &obj.as_ref())
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

    pub fn delete(&mut self, uuid: Uuid) -> Result<()> {
        self.0.delete(uuid)
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