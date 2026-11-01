use crate::model_test::ModelResult;

const ROW_H: i32 = 44;
const PAD: i32 = 24;
const HEAD_H: i32 = 90;
const COL_MODEL_X: i32 = PAD + 12;
const COL_STATUS_X: i32 = 560;
const COL_TIME_X: i32 = 700;

/// 将测试结果渲染为 SVG 字符串
fn build_svg(results: &[ModelResult]) -> String {
    let rows = results.len() as i32;
    let width = 960;
    let height = HEAD_H + rows * ROW_H + PAD;

    let mut body = String::new();
    body.push_str(&format!(
        r#"<rect x="0" y="0" width="{width}" height="{height}" fill="#0f172a"/>
        <text x="{x}" y="44" font-size="26" font-weight="bold" fill="#e2e8f0">模型 API 测速报告</text>
        <text x="{x}" y="72" font-size="14" fill="#94a3b8">共 {n} 个模型 · 随机题防空回 · 单位 ms</text>
        <line x1="{pad}" y1="{head}" x2="{line_end}" y2="{head}" stroke="#334155" stroke-width="1"/>
        "#,
        width = width,
        height = height,
        x = COL_MODEL_X,
        n = rows,
        pad = PAD,
        head = HEAD_H,
        line_end = width - PAD,
    ));

    // 表头
    body.push_str(&format!(
        r#"<text x="{mx}" y="{ty}" font-size="15" fill="#64748b">模型</text>
        <text x="{sx}" y="{ty}" font-size="15" fill="#64748b">状态</text>
        <text x="{tx}" y="{ty}" font-size="15" fill="#64748b">耗时/结果</text>"#,
        mx = COL_MODEL_X,
        sx = COL_STATUS_X,
        tx = COL_TIME_X,
        ty = HEAD_H + 4,
    ));

    for (i, r) in results.iter().enumerate() {
        let y = HEAD_H + (i as i32) * ROW_H + 4;
        let row_top = HEAD_H + (i as i32) * ROW_H;
        // 斑马纹
        if i % 2 == 0 {
            body.push_str(&format!(
                r#"<rect x="8" y="{row_top}" width="{w}" height="{ROW_H}" fill="#1e293b"/>"#,
                row_top = row_top,
                w = width - 16,
                ROW_H = ROW_H
            ));
        }

        // 状态徽章色 + 文本
        let (badge_color, status_text) = if r.ok {
            ("#16a34a", "成功")
        } else if r.error.as_deref().map(|e| e.contains("空回")).unwrap_or(false) {
            ("#d97706", "空回")
        } else if r.error.as_deref().map(|e| e.contains("超时")).unwrap_or(false) {
            ("#dc2626", "超时")
        } else {
            ("#dc2626", "错误")
        };

        // 结果文本
        let result_text = if r.ok {
            format!("{} ms", r.latency_ms.unwrap_or(0))
        } else {
            let e = r.error.clone().unwrap_or_default();
            // 截断错误信息
            if e.chars().count() > 40 {
                e.chars().take(40).collect::<String>() + "…"
            } else {
                e
            }
        };

        body.push_str(&format!(
            r#"<text x="{mx}" y="{ty}" font-size="15" fill="#e2e8f0">{model}</text>
            <rect x="{sx}" y="{badge_y}" width="64" height="24" rx="12" fill="{badge_color}"/>
            <text x="{sx_center}" y="{badge_text_y}" font-size="13" fill="#ffffff" text-anchor="middle">{status}</text>
            <text x="{tx}" y="{ty}" font-size="14" fill="{result_color}">{result}</text>"#,
            mx = COL_MODEL_X,
            ty = y + 24,
            model = escape_xml(&r.model),
            sx = COL_STATUS_X,
            badge_y = row_top + 10,
            badge_color = badge_color,
            sx_center = COL_STATUS_X + 32,
            badge_text_y = row_top + 27,
            status = status_text,
            tx = COL_TIME_X,
            result_color = if r.ok { "#86efac" } else { "#fca5a5" },
            result = escape_xml(&result_text),
        ));
    }

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">{body}</svg>"#,
        width = width,
        height = height,
        body = body
    )
}

/// 将结果渲染为 PNG data URL（base64）
pub fn render_results_png(results: &[ModelResult]) -> String {
    let svg = build_svg(results);
    // 尝试用 resvg 光栅化；若字体缺失则返回空串（前端可降级显示表格）
    match svg_to_png(&svg) {
        Ok(bytes) => {
            let b64 = base64_encode(&bytes);
            format!("data:image/png;base64,{}", b64)
        }
        Err(_) => String::new(),
    }
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>, ()> {
    use resvg::tiny_skia;
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|_| ())?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).ok_or(())?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|_| ())
}

fn base64_encode(data: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(data)
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', """)
        .replace('\'', "'")
}