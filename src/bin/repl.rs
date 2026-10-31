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
    let mut db = TypedDatabase::new("./fs.db")?;

    let obj = db.insert(serde_json::json! {{
        "success": true,
        "user": {
            "name": "JCake",
            "email": "jakieschneider13@gmail.com"
        }
    }})?;
    dbg!(db.search::<Message>(obj)?);

    Ok(())
}