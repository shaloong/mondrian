use mondrian_app::app::MondrianApp;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    // 日志初始化：RUST_LOG 环境变量覆盖，默认 info
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn,naga=warn")),
        )
        .with_target(true)
        .init();

    tracing::info!("Mondrian v{} 启动", env!("CARGO_PKG_VERSION"));

    // Tokio runtime — 供后台任务（解码、AI调用、导出）使用
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;

    // 将 runtime handle 存入 thread-local，供后台任务调度
    let _guard = rt.enter();

    let startup_size = mondrian_app::ui::theme::tokens::startup_viewport_size();

    // eframe 原生窗口配置
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mondrian")
            // 启动时先进入引导窗口模式，主工作区尺寸会在打开项目后恢复。
            .with_inner_size(startup_size)
            .with_min_inner_size(startup_size)
            .with_max_inner_size(startup_size)
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_icon(load_icon()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "mondrian",
        native_options,
        Box::new(|cc| Ok(Box::new(MondrianApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("UI error: {e}"))?;

    Ok(())
}

/// 从编译期嵌入的 PNG 字节加载窗口图标
fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/app-ico.png");
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData { rgba: rgba.into_raw(), width, height }
        }
        Err(err) => {
            tracing::warn!("加载窗口图标失败: {err}");
            egui::IconData::default()
        }
    }
}
