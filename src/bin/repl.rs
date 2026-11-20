use db::TypedDatabase;
use db::items;
use db::keys;
use serde::Deserialize;
use serde::Serialize;
use util::binary::Timestamp;
use uuid::Uuid;

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
    let mut db = TypedDatabase::new("./test/fs.db")?.with_mapper::<Item>(|obj| match obj {
        Item::Message(msg) => keys![msg.created, msg.from, msg.to],
        Item::User(user) => keys![
            user.name.to_lowercase(),
            user.email.to_lowercase(),
            user.oidc_uuid
        ],
    });

    let obj = db.insert(
        User {
            name: "JCake".to_string(),
            email: "jakieschneider13@gmail.com".to_string(),
            registered: chrono::Utc::now(),
            oidc_uuid: Default::default(),
        }
        .into_item(),
    )?;

    // db.delete(obj)?;

    let res = db.query::<Item>("JCake")?;

    dbg!(res);

    Ok(())
}
