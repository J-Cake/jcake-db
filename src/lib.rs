#![feature(gen_blocks)]
extern crate core;

mod file;
mod page;
mod object;
mod content_index;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
pub use file::BlockDriver;
pub use util::Device;
pub use util::Error;
pub use util::Reader;
pub use util::Writer;
pub use util::Result;
pub use util::traits::ToBytes;
use crate::content_index::ContentIndexCx;
use crate::file::blank;
use crate::file::open;
use crate::object::Object;

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
    mapper: Box<dyn Fn(&[u8]) -> Vec<Box<[u8]>>>,
    content_index: ContentIndexCx
    // content_index: HashMap<Box<[u8]>, Vec<Uuid>>
}

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub fn insert(&mut self, data: impl Serialize) -> Result<Uuid> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        let uuid = self.backing.insert(&obj.as_ref())?;

        self.content_index.add(uuid, (self.mapper)(obj.as_ref()));

        Ok(uuid)
    }

    pub fn upsert(&mut self, uuid: Uuid, data: impl Serialize) -> Result<()> {
        let obj = jsonb::to_owned_jsonb(&data)?;
        self.backing.upsert(uuid, &obj.as_ref())?;
        self.content_index.add(uuid, (self.mapper)(obj.as_ref()));

        Ok(())
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

    pub fn query<T: DeserializeOwned>(&self, search: impl ToBytes) -> Result<Vec<T>> {
        let _ct = self.content_index.iter()
            .map(|(key, value)| (String::from_utf8_lossy(key), value))
            .collect::<std::collections::HashMap<_, _>>();

        let Some(matches) = self.content_index.get(&search.to_bytes()) else {
            return Ok(vec![]);
        };

        matches.into_iter()
            .filter_map(|i| self.backing.fetch(*i).transpose())
            .map(|i| i.and_then(|i| i.try_into_parsed()))
            .collect()
    }

    pub fn delete(&mut self, uuid: Uuid) -> Result<()> {
        self.backing.delete(uuid)?;
        self.content_index.del(uuid);
        Ok(())
    }

    pub fn with_mapper<Type: Serialize + DeserializeOwned>(mut self, mapper: impl Fn(&Type) -> Vec<Box<[u8]>> + 'static) -> Self {
        self.mapper = Box::new(move |obj| {
            let obj = jsonb::RawJsonb::new(obj);
            let obj = match jsonb::from_raw_jsonb(&obj) {
                Ok(ok) => ok,
                Err(err) => {
                    dbg!(format!("Indexing object failed: {err}"));
                    return vec![];
                }
            };

            return mapper(&obj);
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
                content_index: Default::default(),
            })
        } else {
            Ok(Self {
                backing: blank(path)?,
                mapper: no_index(),
                content_index: Default::default(),
            })
        }
    }
}

fn no_index() -> Box<dyn Fn(&[u8]) -> Vec<Box<[u8]>>> {
    Box::new(move |_| vec![])
}

#[macro_export]
macro_rules! items {
    ($( $tag:literal => $(#[$meta:meta])* struct $name:ident { $($body:tt)* } )*) => {
        $(
            #[derive(Debug, Serialize, Deserialize)]
            $(#[$meta])*
            pub struct $name { $($body)* }

            impl $name {
                pub fn into_item(self) -> Item {
                    Item::$name(self)
                }
            }
        )*

        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "type")]
        pub enum Item {
            $( #[serde(rename = $tag)] $name($name), )*
        }
    };
}

// pub enum OwnedOrBorrowed<'a> {
//     Borrowed(&'a dyn ToBytes),
//     Owned(Box<dyn ToBytes + 'a>)
// }
//
// impl<'a, T: ToBytes> From<&'a T> for OwnedOrBorrowed<'a> {
//     fn from(value: &'a T) -> Self {
//         Self::Borrowed(value)
//     }
// }
//
// impl<'a> From<Box<dyn ToBytes + 'a>> for OwnedOrBorrowed<'a> {
//     fn from(value: Box<dyn ToBytes + 'a>) -> Self {
//         Self::Owned(value)
//     }
// }

#[macro_export]
macro_rules! keys {
    ($($e:expr),* $(,)?) => {
        vec![$( ::db::ToBytes::to_bytes(&$e) ),*]
    };
}