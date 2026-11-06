use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use util::binary::Fixed;
use util::Device;
use crate::object::ObjectKind;
use crate::{BlockDriver, TypedDatabase};
use crate::file::Offset;
use crate::page::{Page, PageHeader};

#[derive(Debug, Serialize, Deserialize)]
struct ContentIndex(Vec<IndexEntry>);

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum IndexEntry {
    Add {
        data: Vec<String>,
        uuid: Uuid
    },
    Remove {
        uuid: Uuid
    }
}

impl<Dev: Device> TypedDatabase<BlockDriver<Dev>> {
    pub(crate) fn into_indexed(mut self) -> Self {
        let mut reader = self.backing.backing.reader();
        // content => UUID
        let mut forward_index = HashMap::<String, Vec<Uuid>>::new();
        // UUID => content
        let mut backward_index = HashMap::<Uuid, Vec<String>>::new();

        let mut last_index = None;

        for record in self.backing.iter() {
            if record.kind != ObjectKind::Index {
                continue;
            }

            last_index = Some(record.clone());

            reader.seek_to(record.position);
            let Ok(record) = record.into_object(&mut reader) else {
                continue;
            };

            let Ok(index) = record.try_into_parsed::<ContentIndex>() else {
                continue;
            };

            for entry in index.0 {
                let (IndexEntry::Add { uuid, .. } | IndexEntry::Remove { uuid }) = entry;

                if let Some(content) = backward_index.remove(&uuid) {
                    for bucket in content {
                        if let Some(forward) = forward_index.get_mut(&bucket) {
                            *forward = forward.into_iter()
                                .filter_map(|i| if *i == uuid { Some(*i) } else { None })
                                .collect();
                        }
                    }
                }

                if let IndexEntry::Add { data, uuid } = entry {
                    for content in &data {
                        forward_index.entry(content.clone())
                            .and_modify(|vec| vec.push(uuid))
                            .or_insert(vec![uuid]);
                    }

                    backward_index.insert(uuid, data);
                }
            }
        }

        let start_iterative = last_index
            .and_then(|i| self.backing.get_location(i.uuid))
            .map(|i| i.latest())
            .unwrap_or((0, (PageHeader::SIZE as Offset).next_multiple_of(Page::ALIGNMENT as Offset)));

        // TODO: iterate starting here.

        self.content_index = forward_index;
        self
    }
}