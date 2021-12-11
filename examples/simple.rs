use std::{sync::Arc, time::Duration};

use futures_lite::StreamExt;
use pm::{Command, Manager};
use runtime::{Runtime, SmolGlobalRuntime};

fn main() {
    pretty_env_logger::init();

    let runtime = SmolGlobalRuntime;

    let manager = Manager::new(runtime);

    runtime.block_on(async move {
        let mut events = manager.watch();

        runtime.spawn(async move {
            //
            while let Some(event) = events.next().await {
                println!("Event {:?}", event);
            }
        });

        let pid = manager.spawn(Command::new("sleep").arg("20")).await;

        // smol::Timer::after(Duration::from_secs(2)).await;

        // manager.stop(&pid).await;

        // smol::Timer::after(Duration::from_secs(1)).await;

        println!("Process {:?}", manager.list().await);

        // smol::Timer::after(Duration::from_secs(1)).await;

        manager.wait().await;
    })
}
