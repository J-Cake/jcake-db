#![feature(map_try_insert)]
#![feature(gen_blocks)]
extern crate core;

mod file;
mod page;
mod object;
mod content_index;

use std::collections::HashMap;
use std::fs;
use std::fs::read;
use std::hash::{Hash, Hasher};
use std::io::Bytes;
use std::path::Path;
use serde::Serialize;
use serde::de::{DeserializeOwned, DeserializeSeed};
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
use crate::object::{Object, ObjectKind};

pub trait ObjectStore: Sized {
    fn compact_db(&mut self) -> Result<()>;
    fn entries(&self) -> impl Iterator<Item=Object>;

    fn fetch(&self, id: Uuid) -> Result<Option<Object>>;
    fn insert(&mut self, data: &[u8]) -> Result<Uuid>;
    fn upsert(&mut self, id: Uuid, data: &[u8]) -> Result<()>;
    fn delete(&mut self, id: Uuid) -> Result<()>;
}

pub struct TypedDatabase<Db: ObjectStore> {
    backing: Db,
    mapper: Box<dyn Fn(&[u8]) -> Vec<Vec<u8>>>,
    content_index: HashMap<String, Vec<Uuid>>
}

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub fn insert(&mut self, data: impl Serialize) -> Result<Uuid> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.backing.insert(&obj.as_ref())
    }

    pub fn upsert(&mut self, uuid: Uuid, data: impl Serialize) -> Result<()> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.backing.upsert(uuid, &obj.as_ref())
    }

    pub fn fetch<T: DeserializeOwned>(&self, id: Uuid) -> Result<Option<T>> {
        let Some(obj) = self.backing.fetch(id)? else {
            return Ok(None);
        };
        let raw = jsonb::RawJsonb::new(&obj);
        let obj = jsonb::from_raw_jsonb(&raw)
            .map_err(Into::<Error>::into)?;

        Ok(Some(obj))
    }

    pub fn query<T: DeserializeOwned>(&self, search: impl AsRef<str>) -> Result<Vec<T>> {
        let Some(matches) = self.content_index.get(search.as_ref()) else {
            return Ok(vec![]);
        };

        matches.into_iter()
            .filter_map(|i| self.backing.fetch(*i).transpose())
            .map(|i| i.and_then(|i| i.try_into_parsed()))
            .collect()
    }

    pub fn delete(&mut self, uuid: Uuid) -> Result<()> {
        self.backing.delete(uuid)
    }

    pub fn with_mapper<Type: Serialize + DeserializeOwned>(mut self, mapper: impl Fn(&Type) -> Vec<&dyn ToString> + 'static) -> Self {
        self.mapper = Box::new(move |obj| {
            let obj = jsonb::RawJsonb::new(obj);
            let obj = match jsonb::from_raw_jsonb(&obj) {
                Ok(ok) => ok,
                Err(err) => {
                    dbg!(format!("Indexing object failed: {err}"));
                    return vec![];
                }
            };
            let res = mapper(&obj);

            res.into_iter()
                .map(ToString::to_string)
                .map(String::into_bytes)
                .collect()
        });

        self.into_indexed()
    }
}

impl TypedDatabase<BlockDriver<fs::File>> {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            Ok(Self {
                backing: open(path)?,
                mapper: no_index(),
                content_index: HashMap::new(),
            })
        } else {
            Ok(Self {
                backing: blank(path)?,
                mapper: no_index(),
                content_index: HashMap::new(),
            })
        }
    }
}

fn no_index() -> Box<dyn Fn(&[u8]) -> Vec<Vec<u8>>> {
    Box::new(move |_| vec![])
}

#[macro_export]
macro_rules! items {
    ($( $tag:literal => $(#[$meta:meta])* struct $name:ident { $($body:tt)* } )*) => {
        $(
            #[derive(Debug, Serialize, Deserialize)]
            $(#[$meta])*
            pub struct $name { $($body)* }
        )*

        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "type")]
        pub enum Item {
            $( #[serde(rename = $tag)] $name($name), )*
        }
    };
}

#[macro_export]
macro_rules! keys {
    ($($e:expr),* $(,)?) => {
        vec![$( &$e as &dyn ::std::string::ToString ),*]
    };
}