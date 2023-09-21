use anyhow::Result;
use graphql_client::GraphQLQuery;

use crate::{cache::Cache, cli::UploadDrop, config::Config, runtime};

#[allow(clippy::upper_case_acronyms)]
type UUID = uuid::Uuid;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/queries/schema.graphql",
    query_path = "src/queries/queue-mint-to-drop.graphql",
    response_derives = "Debug"
)]
struct QueueMintToDrop;

pub fn run(config: &Config, args: UploadDrop) -> Result<()> {
    let UploadDrop { drop_id, asset_dir } = args;
    let mut cache = Cache::load(&asset_dir)?;

    let uploads = cache.asset_uploads_mut();

    runtime()?.block_on(async move {
        let client = config.graphql_client()?;

        let res = config
            .post_graphql::<QueueMintToDrop>(&client, queue_mint_to_drop::Variables {
                in_: queue_mint_to_drop::QueueMintToDropInput {
                    drop: drop_id,
                    metadata_json: queue_mint_to_drop::MetadataJsonInput {
                        name: "hi".into(),
                        symbol: "HI".into(),
                        description: "help".into(),
                        image: "data:text/plain;help".into(),
                        animation_url: None,
                        collection: None,
                        attributes: vec![],
                        external_url: None,
                        properties: None,
                    },
                },
            })
            .await;

        println!("{res:?}");

        Result::<_>::Ok(())
    })?;

    println!("{:?}", config.upload_endpoint());

    cache.save(asset_dir)
}
