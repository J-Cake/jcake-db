use uuid::Uuid;

pub(crate) struct Block {
    uuid: Uuid,
    object: Uuid,
    continuation: Vec<Uuid>,
    length: u32,
    crc: u32,
    deleted: bool,
}