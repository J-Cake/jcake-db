use serde::Serialize;
use serde::Deserialize;
use uuid::Uuid;
use db::TypedDatabase;

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    message: String
}

type Result<T, Error = db::Error> = core::result::Result<T, Error>;

pub fn main() -> Result<()> {
    let db = TypedDatabase::new("/tmp/fs.db")?;

    dbg!(db.search::<Message>(Uuid::new_v4())?);

    Ok(())
}