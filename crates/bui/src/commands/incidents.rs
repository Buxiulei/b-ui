//! `bui incidents [--json] [-n N]`：日志哨兵的事件，新的在前（spec §5.7）。
//! 守护进程在跑就经 socket 读，否则直接读 `runtime.json`（与 `bui status` 同一退化口径）。

use crate::modules::sentinel::incidents;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::path::PathBuf;

pub async fn run(json: bool, n: usize, paths: Paths, socket: PathBuf) -> Result<()> {
    let (list, live) = incidents::load_recent(&socket, &paths, n).await;
    if json {
        let v = serde_json::json!({
            "incidents": list,
            "source": if live { "daemon" } else { "runtime.json" },
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        if !live {
            eprintln!("守护进程未运行，以下读自 runtime.json");
        }
        println!("{}", incidents::format_list(&list));
    }
    Ok(())
}
