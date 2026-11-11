use serde::Serialize;
use serde::Deserialize;
use uuid::Uuid;
use db::TypedDatabase;
use db::items;
use db::keys;
use util::binary::Timestamp;

items! {
    "msg" => struct Message {
        created: Timestamp,
        contents: String,
        from: Uuid,
        to: Uuid,
    }

    "user" => struct User {
        name: String,
        email: String,
        registered: Timestamp,
        oidc_uuid: Uuid,
    }
}

type Result<T, Error = db::Error> = core::result::Result<T, Error>;

pub fn main() -> Result<()> {
    let mut db = TypedDatabase::new("./fs.db")?
        .with_mapper::<Item>(|obj| match obj {
            Item::Message(msg) => keys![msg.created, msg.from, msg.to],
            Item::User(user) => keys![user.name, user.email, user.oidc_uuid]
        });

    let obj = db.insert(User {
        name: "JCake".to_string(),
        email: "jakieschneider13@gmail.com".to_string(),
        registered: chrono::Utc::now(),
        oidc_uuid: Default::default(),
    }.into_item())?;

    db.delete(obj)?;

    let res = db.query::<Item>("JCake")?;

    dbg!(res);

    Ok(())
}