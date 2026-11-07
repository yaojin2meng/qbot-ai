use crate::model_test::ModelResult;

const ROW_H: i32 = 44;
const PAD: i32 = 24;
const HEAD_H: i32 = 90;
const TABLE_H: i32 = 32;
const COL_MODEL_X: i32 = PAD + 12;
const COL_STATUS_X: i32 = 560;
const COL_TIME_X: i32 = 700;

/// 将测试结果渲染为 SVG 字符串
pub fn build_svg(results: &[ModelResult]) -> String {
    let rows = results.len() as i32;
    let width = 960;
    let height = HEAD_H + TABLE_H + rows * ROW_H + PAD;

    let mut body = String::new();
    body.push_str(&format!(
        r##"<rect x="0" y="0" width="{width}" height="{height}" fill="#0f172a" />
        <text x="{x}" y="44" font-size="26" font-weight="bold" fill="#e2e8f0" >模型 API 测速报告</text>
        <text x="{x}" y="72" font-size="14" fill="#94a3b8" >共 {n} 个模型 · 随机题防空回 · 单位 ms</text>
        <line x1="{pad}" y1="{head}" x2="{line_end}" y2="{head}" stroke="#334155" stroke-width="1" />
        "##,
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
        r##"<text x="{mx}" y="{ty}" font-size="15" fill="#64748b" >模型</text>
        <text x="{sx}" y="{ty}" font-size="15" fill="#64748b" >状态</text>
        <text x="{tx}" y="{ty}" font-size="15" fill="#64748b" >耗时/结果</text>"##,
        mx = COL_MODEL_X,
        sx = COL_STATUS_X,
        tx = COL_TIME_X,
        ty = HEAD_H + 23,
    ));

    for (i, r) in results.iter().enumerate() {
        let y = HEAD_H + TABLE_H + (i as i32) * ROW_H + 4;
        let row_top = HEAD_H + TABLE_H + (i as i32) * ROW_H;
        // 斑马纹
        if i % 2 == 0 {
            body.push_str(&format!(
                r##"<rect x="8" y="{row_top}" width="{w}" height="{ROW_H}" fill="#1e293b" />"##,
                row_top = row_top,
                w = width - 16,
                ROW_H = ROW_H
            ));
        }

        // 状态徽章色 + 文本
        let (badge_color, status_text) = if r.ok {
            ("#16a34a", "成功")
        } else if r
            .error
            .as_deref()
            .map(|e| e.contains("空回"))
            .unwrap_or(false)
        {
            ("#d97706", "空回")
        } else if r
            .error
            .as_deref()
            .map(|e| e.contains("超时"))
            .unwrap_or(false)
        {
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
            r##"<text x="{mx}" y="{ty}" font-size="15" fill="#e2e8f0" >{model}</text>
            <rect x="{sx}" y="{badge_y}" width="64" height="24" rx="12" fill="{badge_color}" />
            <text x="{sx_center}" y="{badge_text_y}" font-size="13" fill="#ffffff" text-anchor="middle" >{status}</text>
            <text x="{tx}" y="{ty}" font-size="14" fill="{result_color}" >{result}</text>"##,
            mx = COL_MODEL_X,
            ty = y + 24,
            model = escape_xml(&r.model.chars().take(48).collect::<String>()),
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
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">{body}</svg>"##,
        width = width,
        height = height,
        body = body
    )
}

/// 将结果渲染为 PNG 字节数组
pub fn render_results_png_bytes(results: &[ModelResult]) -> Option<Vec<u8>> {
    let svg = build_svg(results);
    svg_to_png(&svg).ok()
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>, ()> {
    use resvg::tiny_skia;
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb_mut()
        .load_font_data(include_bytes!("../assets/report-font.ttf").to_vec());
    opt.font_family = "Noto Sans SC".into();
    if opt.fontdb.faces().next().is_none() {
        return Err(());
    }
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|_| ())?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).ok_or(())?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|_| ())
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("\x26\x71\x75\x6f\x74\x3b"),
            '\'' => out.push_str("\x26\x61\x70\x6f\x73\x3b"),
            _ => out.push(c),
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_font_and_png() {
        let result = ModelResult {
            model: "test-model".into(),
            ok: true,
            latency_ms: Some(123),
            error: None,
            prompt: String::new(),
            answer: String::new(),
        };
        let bytes = render_results_png_bytes(&[result]).expect("embedded-font PNG should render");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(escape_xml("<&>"), "&lt;&amp;&gt;");
    }
}
