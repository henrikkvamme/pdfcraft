use std::sync::Arc;

use pdfcraft_cos::{Object, SaveOptions, write_full, write_incremental};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

use super::*;
use crate::{Edit, NewAnnotation, Shape, Style};

const SECRET: &str = "SECRET42";

/// A one-revision PDF of `objs` (object 1 the catalog), each written as `n 0 obj … endobj`.
fn pdf(objs: &[String], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, body: &str) -> String {
    format!("<< /Length {} {dict} >>\nstream\n{body}\nendstream", body.len())
}

/// A 612 × 300 page drawing `body`, with Helvetica as /F1 (object 4) and `resources` added;
/// `page_extra` goes into the page dictionary, `catalog_extra` into the catalog, and `more` are
/// objects 6, 7, ….
fn page(resources: &str, body: &str, page_extra: &str, catalog_extra: &str, more: &[String]) -> Vec<u8> {
    let mut objs = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 300] >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> {resources} >> /Contents 5 0 R {page_extra} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        stream("", body),
    ];
    objs.extend_from_slice(more);
    pdf(&objs, "")
}

const VISIBLE: &str = "BT /F1 14 Tf 40 200 Td (Client SECRET42 Holdings) Tj ET BT /F1 12 Tf 40 120 Td (This line stays.) Tj ET";

/// Mark every place the page's text layer shows `needle`.
fn mark(s: &mut Session, id: DocId, needle: &str) {
    let doc = s.get(id).unwrap();
    let mut r = PageRenderer::new(doc.bytes.clone(), RenderConfig::default());
    let text = r.render(RenderRequest { page: 0, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text.expect("the page has text");
    let info = &doc.info.pages[0];
    let quads: Vec<_> = text
        .find(needle)
        .into_iter()
        .flat_map(|range| text.line_rects(range))
        .map(|v| pdfcraft_annot::rect_quad(info.view_rect_to_user(v).map(f64::from)))
        .collect();
    assert!(!quads.is_empty(), "the page shows {needle}");
    mark_quads(s, id, quads);
}

/// Where the page's text layer shows `needle`, in view space (points, y down).
fn shown_rect(s: &Session, id: DocId, needle: &str) -> [f32; 4] {
    let doc = s.get(id).unwrap();
    let mut r = PageRenderer::new(doc.bytes.clone(), RenderConfig::default());
    let text = r.render(RenderRequest { page: 0, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text.expect("the page has text");
    let range = text.find(needle).pop().expect("the page shows the word");
    text.line_rects(range).pop().expect("a line")
}

fn mark_quads(s: &mut Session, id: DocId, quads: Vec<[f64; 8]>) {
    let shape = Shape::Redact { quads, overlay: String::new(), look: Default::default() };
    let style = Style::default_for(&shape);
    s.apply(id, Edit::AddAnnotation(NewAnnotation { page: 0, style, shape, contents: String::new(), author: "Test".into() })).unwrap();
}

fn copy(s: &Session, id: DocId) -> SecureCopy {
    copy_at(s, id, 150.0)
}

fn copy_at(s: &Session, id: DocId, dpi: f32) -> SecureCopy {
    s.secure_copy_job(id, SecureOptions { dpi, ocr: false }).unwrap().run(None, |_, _| true).unwrap()
}

/// Every decoded stream and every string of `bytes`, and the bytes themselves.
fn holds(bytes: &[u8], needle: &str) -> Option<String> {
    let n = needle.as_bytes();
    let has = |d: &[u8]| d.windows(n.len()).any(|w| w == n);
    if has(bytes) {
        return Some("raw bytes".into());
    }
    let doc = pdfcraft_cos::Document::open(Arc::new(bytes.to_vec())).unwrap();
    for (r, o) in doc.scan_objects() {
        if let Object::Stream(st) = &*o
            && st.decoded().is_ok_and(|d| has(&d))
        {
            return Some(format!("stream {}", r.num));
        }
    }
    None
}

fn render(bytes: &[u8], scale: f32) -> (u32, u32, Vec<u8>) {
    let mut r = PageRenderer::new(Arc::new(bytes.to_vec()), RenderConfig::default());
    let p = r.render(RenderRequest { page: 0, scale, ..Default::default() });
    assert!(p.error.is_none(), "{:?}", p.error);
    (p.width, p.height, p.rgba.to_vec())
}

fn px(img: &(u32, u32, Vec<u8>), x: f32, y: f32) -> [u8; 4] {
    let (w, _, d) = img;
    let i = ((y as usize) * (*w as usize) + x as usize) * 4;
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

/// Each place the word hides in a source file: the secure copy has none of them, and the box
/// is drawn where the word was shown.
#[test]
fn nothing_of_the_source_reaches_the_copy() {
    let hidden_layer = page(
        "/Properties << /L1 6 0 R >>",
        &format!("{VISIBLE} /OC /L1 BDC BT /F1 12 Tf 40 60 Td (Layer note {SECRET}) Tj ET EMC"),
        "",
        "/OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >>",
        &["<< /Type /OCG /Name (Pricing) >>".to_string()],
    );
    let pattern = page(
        "/Pattern << /P1 6 0 R >>",
        &format!("q /Pattern cs /P1 scn 90 190 120 30 re f Q {VISIBLE}"),
        "",
        "",
        &[stream(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 60 12] /XStep 60 /YStep 12 /Resources << /Font << /F1 4 0 R >> >>",
            &format!("BT /F1 6 Tf 1 2 Td ({SECRET}) Tj ET"),
        )],
    );
    let actual_text = page("", &format!("/Span << /ActualText ({SECRET} Holdings) >> BDC {VISIBLE} EMC"), "", "", &[]);
    let everywhere = {
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /Metadata 9 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 300] >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R /Annots [6 0 R] >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
            stream("", VISIBLE),
            format!("<< /Type /Annot /Subtype /Text /Rect [500 250 520 270] /Contents (Check {SECRET}) >>"),
            "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>".to_string(),
            format!("<< /Title ({SECRET} section) /Parent 7 0 R /Dest [3 0 R /Fit] >>"),
            stream("/Type /Metadata /Subtype /XML", &format!("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><dc:title>{SECRET}</dc:title></x:xmpmeta>")),
            format!("<< /Title ({SECRET} bid) >>"),
        ];
        objs.truncate(10);
        pdf(&objs, "/Info 10 0 R")
    };
    let nested_form = page(
        "/XObject << /X1 6 0 R >>",
        "BT /F1 14 Tf 40 200 Td (Client) Tj ET q 1 0 0 1 95 200 cm /X1 Do Q BT /F1 12 Tf 40 120 Td (This line stays.) Tj ET",
        "",
        "",
        &[
            stream("/Type /XObject /Subtype /Form /BBox [0 -5 120 20] /Resources << /XObject << /X2 7 0 R >> >>", "/X2 Do"),
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 -5 120 20] /Resources << /Font << /F1 4 0 R >> >>",
                &format!("BT /F1 14 Tf 0 0 Td ({SECRET}) Tj ET"),
            ),
        ],
    );
    for (name, file) in
        [("hidden layer", hidden_layer), ("pattern", pattern), ("actual text", actual_text), ("everywhere", everywhere), ("nested form", nested_form)]
    {
        let mut s = Session::new().with_clock(|| 1_700_000_000);
        let id = s.open("t.pdf", None, Arc::new(file), None).unwrap();
        assert!(holds(&s.get(id).unwrap().bytes, SECRET).is_some(), "{name}: the source holds the word");
        let word = shown_rect(&s, id, SECRET);
        mark(&mut s, id, SECRET);
        let out = copy(&s, id);
        assert_eq!(holds(&out.bytes, SECRET), None, "{name}: the copy holds the word");
        assert_eq!(check(&out.bytes), Ok(()), "{name}");
        assert_eq!(out.report.pages.len(), 1);
        assert_eq!(out.report.marks, 1);
        // The source keeps its mark: nothing changed there.
        assert_eq!(s.get(id).unwrap().redaction_marks(), 1, "{name}");
        // 612 × 300 points, at 150 dpi.
        let img = render(&out.bytes, 1.0);
        assert_eq!((img.0, img.1), (612, 300), "{name}");
        // Every pixel where the word was shown is the box's, and the page around it is still drawn.
        let dark = |p: [u8; 4]| p[0] < 60 && p[1] < 60 && p[2] < 60;
        let [x0, y0, x1, y1] = word;
        for y in (y0.ceil() as u32)..(y1.floor() as u32) {
            for x in (x0.ceil() as u32)..(x1.floor() as u32) {
                assert!(dark(px(&img, x as f32, y as f32)), "{name}: ({x}, {y}) inside the word is not covered");
            }
        }
        assert!(!dark(px(&img, 300.0, 40.0)), "{name}: the page is dark where nothing was marked");
    }
}

/// A red square drawn on a page with a crop box, `/Rotate` and `/UserUnit`, marked exactly: no
/// red pixel is left, because the renderer places the box as it places the square.
#[test]
fn boxes_cover_what_the_page_draws_whatever_its_geometry() {
    for (crop, user_unit, rotate, square, dpi) in [
        ([0.0, 0.0, 612.0, 300.0], 1.0, 0, [100.0, 100.0, 160.0, 140.0], 150.0),
        ([50.0, 20.0, 400.0, 280.0], 1.0, 90, [100.0, 100.0, 160.0, 140.0], 150.0),
        ([10.0, 10.0, 300.0, 200.0], 2.0, 270, [40.0, 30.0, 70.0, 50.0], 150.0),
        ([0.0, 0.0, 10.0, 8.0], 50.0, 90, [3.0, 4.0, 5.0, 6.0], 150.0),
    ] {
        let [x0, y0, x1, y1] = square;
        let body = format!("1 0 0 rg {x0} {y0} {} {} re f", x1 - x0, y1 - y0);
        let extra = format!("/CropBox [{} {} {} {}] /Rotate {rotate} /UserUnit {user_unit}", crop[0], crop[1], crop[2], crop[3]);
        let file = page("", &body, &extra, "", &[]);
        let mut s = Session::new();
        let id = s.open("g.pdf", None, Arc::new(file), None).unwrap();
        mark_quads(&mut s, id, vec![pdfcraft_annot::rect_quad(square)]);
        let out = copy_at(&s, id, dpi);
        let img = render(&out.bytes, dpi / 72.0);
        // Any red tint: an edge pixel the square only partly covers is pink, not red.
        let red = img.2.as_chunks::<4>().0.iter().filter(|p| i32::from(p[0]) - i32::from(p[1]) > 24).count();
        let dark = img.2.as_chunks::<4>().0.iter().filter(|p| p[0] < 60 && p[1] < 60 && p[2] < 60).count();
        assert_eq!(red, 0, "crop {crop:?} unit {user_unit} rotate {rotate}: red is left");
        // The box is the square's, not the whole page: the paper beside it is still light.
        let light = img.2.as_chunks::<4>().0.iter().filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200).count();
        assert!(light > img.2.len() / 4 / 4, "crop {crop:?} unit {user_unit} rotate {rotate}: most of the page is dark");
        assert!(dark > 0, "crop {crop:?} unit {user_unit} rotate {rotate}: no box");
    }
}

fn good_copy() -> Vec<u8> {
    let mut s = Session::new();
    let id = s.open("t.pdf", None, Arc::new(page("", VISIBLE, "", "", &[])), None).unwrap();
    mark(&mut s, id, SECRET);
    copy(&s, id).bytes
}

/// The check refuses anything a secure copy does not write.
#[test]
fn the_check_refuses_anything_else() {
    let good = good_copy();
    assert_eq!(check(&good), Ok(()));
    let full = |cos: &pdfcraft_cos::Document| write_full(cos, &SaveOptions { object_streams: false, ..SaveOptions::default() }).unwrap();
    let open = || pdfcraft_cos::Document::open(Arc::new(good.clone())).unwrap();
    let first_page = |cos: &pdfcraft_cos::Document| pdfcraft_model::pages(cos).swap_remove(0).obj;

    let mut cos = open();
    let info = cos.add(Object::Dict(Dict::new()));
    cos.trailer_mut().set(b"Info".to_vec(), Object::Ref(info));
    assert!(check(&full(&cos)).unwrap_err().contains("/Info"));

    let mut cos = open();
    let p = first_page(&cos);
    cos.update_dict(p, |d| d.set(b"Annots".to_vec(), Object::Array(vec![]))).unwrap();
    assert!(check(&full(&cos)).unwrap_err().contains("/Annots"));

    let mut cos = open();
    let root = cos.root().unwrap();
    cos.update_dict(root, |d| d.set(b"Outlines".to_vec(), Object::Dict(Dict::new()))).unwrap();
    assert!(check(&full(&cos)).unwrap_err().contains("/Outlines"));

    // A form XObject in place of the page image.
    let mut cos = open();
    let p = first_page(&cos);
    let mut form = Dict::new();
    form.set(b"Type".to_vec(), Object::name("XObject"));
    form.set(b"Subtype".to_vec(), Object::name("Form"));
    let form = cos.add(Object::Stream(Stream::flate(form, b"")));
    cos.update_dict(p, |d| {
        let mut x = Dict::new();
        x.set(b"Im0".to_vec(), Object::Ref(form));
        let mut res = Dict::new();
        res.set(b"XObject".to_vec(), Object::Dict(x));
        d.set(b"Resources".to_vec(), Object::Dict(res));
    })
    .unwrap();
    assert!(check(&full(&cos)).is_err());

    // A second revision.
    let mut cos = open();
    let p = first_page(&cos);
    cos.update_dict(p, |d| d.set(b"Rotate".to_vec(), Object::Int(0))).unwrap();
    assert!(check(&write_incremental(&cos, &SaveOptions::default()).unwrap()).unwrap_err().contains("revision"));

    // An object nothing uses.
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".into(),
        stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", "abc"),
        stream("", "q 10 0 0 10 0 0 cm /Im0 Do Q"),
    ];
    assert_eq!(check(&pdf(&objs, "")), Ok(()));
    objs.push(format!("({SECRET})"));
    assert!(check(&pdf(&objs, "")).unwrap_err().contains("not used"));
}

/// While secure copies are required, no caller applies redactions in place, alone or in a
/// batch; the secure copy still works.
#[test]
fn a_requirement_stops_every_in_place_apply() {
    let mut s = Session::new();
    let id = s.open("t.pdf", None, Arc::new(page("", VISIBLE, "", "", &[])), None).unwrap();
    mark(&mut s, id, SECRET);
    s.require_secure_redaction(Some("policy.json".into()));
    assert_eq!(s.secure_redaction_required(), Some("policy.json"));
    let refused = |r: Result<(), EditError>| matches!(r, Err(EditError::SecureRedactionRequired(by)) if by == "policy.json");
    assert!(refused(s.apply(id, Edit::ApplyRedactions { pages: None })));
    let nested =
        Edit::Batch { label: "x".into(), edits: vec![Edit::Batch { label: "y".into(), edits: vec![Edit::ApplyRedactions { pages: None }] }] };
    assert!(refused(s.apply(id, nested)));
    assert_eq!(s.get(id).unwrap().redaction_marks(), 1, "the marks are still there");
    let (new, report) = s.secure_redacted_copy(id, SecureOptions { dpi: 100.0, ocr: false }).unwrap();
    assert_eq!(report.pages.len(), 1);
    assert_eq!(s.get(new).unwrap().display_name(), "t (redacted).pdf");
    s.require_secure_redaction(None);
    s.apply(id, Edit::ApplyRedactions { pages: None }).unwrap();
}

#[test]
fn refused_without_marks_or_with_overlay_text() {
    let mut s = Session::new();
    let id = s.open("t.pdf", None, Arc::new(page("", VISIBLE, "", "", &[])), None).unwrap();
    assert!(matches!(s.secure_copy_job(id, SecureOptions::default()), Err(EditError::SecureCopy(SecureError::NoMarks))));
    let shape =
        Shape::Redact { quads: vec![pdfcraft_annot::rect_quad([40.0, 195.0, 200.0, 215.0])], overlay: "(b)(6)".into(), look: Default::default() };
    let style = Style::default_for(&shape);
    s.apply(id, Edit::AddAnnotation(NewAnnotation { page: 0, style, shape, contents: String::new(), author: "T".into() })).unwrap();
    assert!(matches!(s.secure_copy_job(id, SecureOptions::default()), Err(EditError::SecureCopy(SecureError::OverlayText(1)))));
}

/// The words looked for again are the ones under the marks, on a rotated page too (marks are
/// in user space, the text layer in view space).
#[test]
fn marked_words_are_read_from_under_the_marks() {
    for rotate in [0, 90] {
        let body = "BT /F1 14 Tf 40 200 Td (Alpha SECRET42 Omega) Tj ET BT /F1 14 Tf 40 100 Td (Unmarked Words) Tj ET";
        let mut s = Session::new();
        let id = s.open("t.pdf", None, Arc::new(page("", body, &format!("/Rotate {rotate}"), "", &[])), None).unwrap();
        mark(&mut s, id, SECRET);
        let job = s.secure_copy_job(id, SecureOptions::default()).unwrap();
        assert_eq!(job.words, ["secret42"], "rotate {rotate}");
    }
}

#[test]
fn a_marked_word_read_again_in_part_still_counts() {
    assert!(read_again("kestrel7", "kestrel7"));
    assert!(read_again("(kestrel7),", "kestrel7"));
    assert!(read_again("kestrel", "kestrel7"), "OCR dropped a letter");
    assert!(read_again("x-kestrel-y", "kestrel7"));
    assert!(!read_again("kest", "kestrel7"), "too little of it");
    assert!(!read_again("holdings", "kestrel7"));
    assert!(!read_again("the", "thermometer"));
}

/// A colour space the page has or inherits from the page tree (/DefaultRGB that paints
/// nothing) doesn't make the boxes transparent: the mask page has empty resources of its own,
/// and the page tree above it has none.
#[test]
fn inherited_colour_spaces_do_not_hide_the_boxes() {
    let default_rgb = "/ColorSpace << /DefaultRGB [/DeviceN [/None /None /None] /DeviceRGB 6 0 R] >>";
    let function = stream("/FunctionType 4 /Domain [0 1 0 1 0 1] /Range [0 1 0 1 0 1]", "{}");
    let on_page = page(default_rgb, &format!("0 g {VISIBLE}"), "", "", std::slice::from_ref(&function));
    // The page reached through /Kids 2 → 7 → 8, with its /Parent right, missing, or pointing
    // at the empty tree 3.
    let on_tree = |parent: &str| {
        pdf(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
                format!("<< /Type /Pages /Kids [7 0 R] /Count 1 /MediaBox [0 0 612 300] /Resources << {default_rgb} >> >>"),
                "<< /Type /Pages /Kids [] /Count 0 >>".to_string(),
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
                stream("", &format!("0 g {VISIBLE}")),
                function.clone(),
                "<< /Type /Pages /Parent 2 0 R /Kids [8 0 R] /Count 1 >>".to_string(),
                format!("<< /Type /Page {parent} /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            ],
            "",
        )
    };
    for (name, file) in [
        ("on the page", on_page),
        ("on the page tree", on_tree("/Parent 7 0 R")),
        ("on the page tree, no /Parent", on_tree("")),
        ("on the page tree, the wrong /Parent", on_tree("/Parent 3 0 R")),
    ] {
        let mut s = Session::new();
        let id = s.open("cs.pdf", None, Arc::new(file), None).unwrap();
        let word = shown_rect(&s, id, SECRET);
        mark(&mut s, id, SECRET);
        let img = render(&copy(&s, id).bytes, 1.0);
        let [x0, y0, x1, y1] = word;
        let light = ((y0.ceil() as u32)..(y1.floor() as u32))
            .flat_map(|y| ((x0.ceil() as u32)..(x1.floor() as u32)).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&img, x as f32, y as f32)[0] > 60)
            .count();
        assert_eq!(light, 0, "{name}: the box doesn't cover the word");
    }
}

/// A Redact annotation written in place (not an object) can't be found again, so it is
/// refused, even when another mark is listed twice and the counts would agree.
#[test]
fn a_mark_written_in_place_is_refused() {
    let file = page(
        "",
        VISIBLE,
        "/Annots [6 0 R 6 0 R << /Type /Annot /Subtype /Redact /Rect [40 190 200 215] >>]",
        "",
        &["<< /Type /Annot /Subtype /Redact /Rect [40 110 200 135] >>".to_string()],
    );
    let mut s = Session::new();
    let id = s.open("inline.pdf", None, Arc::new(file), None).unwrap();
    assert!(
        matches!(s.secure_copy_job(id, SecureOptions::default()), Err(EditError::SecureCopy(SecureError::UnreadableMark(1)))),
        "{:?}",
        s.secure_copy_job(id, SecureOptions::default()).err()
    );
}

/// The box grows by the unit the renderer uses (a /UserUnit past 75000 counts as 75000), and
/// its edges are written in full: an edge 0.00000049 units past a six-decimal step is not
/// rounded off. (The renderer can't draw such pages exactly, so this is checked on the numbers.)
#[test]
fn boxes_grow_by_the_unit_the_renderer_uses_and_keep_their_edges() {
    let file = page("", "", "/UserUnit 1000000000", "", &[]);
    let cos = pdfcraft_cos::Document::open(Arc::new(file)).unwrap();
    let pg = pdfcraft_model::pages(&cos).swap_remove(0);
    let grow = grow_units(&pg, &cos, 2.0);
    assert!((grow - 1.0 / pdfcraft_model::MAX_USER_UNIT).abs() < 1e-15, "{grow}");
    let mark = Mark {
        page: 0,
        obj: ObjRef { num: 6, generation: 0 },
        rects: vec![[0.0001, 0.0, 0.00020049, 0.0005]],
        fill: None,
        overlay: String::new(),
        look: Default::default(),
    };
    let c = String::from_utf8(mask_content(&[&mark], 0.0).unwrap()).unwrap();
    assert!(c.contains("0.0001 0 0.00010049 0.0005 re f"), "{c}");
}
