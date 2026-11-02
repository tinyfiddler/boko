//! EPUB CFI positions recorded while compiling a chapter.
//!
//! Expected CFIs were produced by foliate-js (`epubcfi.js` `fromRange`) over
//! the same fixtures parsed as `application/xhtml+xml`, so these pin boko to
//! what a browser-based reader writes into reading progress.

use boko::{Chapter, SourcePosition, compile_html};

fn compile(name: &str) -> Chapter {
    let path = format!("{}/tests/fixtures/cfi/{name}", env!("CARGO_MANIFEST_DIR"));
    compile_html(&std::fs::read_to_string(path).unwrap(), &[])
}

fn offset_of(chapter: &Chapter, needle: &str) -> u32 {
    chapter
        .text_buffer()
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in compiled text")) as u32
}

fn text_at(chapter: &Chapter, cfi: &str) -> String {
    match chapter.resolve_cfi(cfi) {
        Some(SourcePosition::Text(o)) => chapter.text_buffer()[o as usize..].to_string(),
        other => panic!("{cfi} resolved to {other:?}"),
    }
}

const CHAPTER: &[(&str, &str)] = &[
    ("One", "/4/2[c1]/2/1:10"),
    ("bright", "/4/2[c1]/4/2/1:0"),
    ("cold day", "/4/2[c1]/4/3:1"),
    ("striking", "/4/2[c1]/4/5:29"),
    ("nose", "/4/2[c1]/4/5:65"),
    ("graph", "/4/2[c1]/8[p^[2^]]/2/2/1:0"),
    ("émoji", "/4/2[c1]/8[p^[2^]]/3:19"),
    ("here.", "/4/2[c1]/8[p^[2^]]/3:28"),
    ("line two", "/4/2[c1]/10/3:0"),
    ("cell B", "/4/2[c1]/12/2/4/1:0"),
    ("Quoted", "/4/2[c1]/14/2/1:0"),
];

const KEPUB: &[(&str, &str)] = &[
    ("Loose", "/4/1:0"),
    (
        "second",
        "/4/2[book-columns]/2[book-inner]/2/4[kobo.1.2]/1:6",
    ),
    (
        "sentence!",
        "/4/2[book-columns]/2[book-inner]/2/4[kobo.1.2]/1:16",
    ),
    ("keeps", "/4/2[book-columns]/2[book-inner]/4/1:9"),
    ("nested", "/4/2[book-columns]/2[book-inner]/6/2/2/2/2/2/1:0"),
    ("odd", "/4/2[book-columns]/2[book-inner]/8/1:161"),
];

#[test]
fn text_cfis_match_foliate() {
    for (fixture, cases) in [("chapter.xhtml", CHAPTER), ("kepub.xhtml", KEPUB)] {
        let chapter = compile(fixture);
        for &(needle, cfi) in cases {
            let at = offset_of(&chapter, needle);
            assert_eq!(
                chapter.text_cfi(at).as_deref(),
                Some(cfi),
                "{fixture}: {needle:?}"
            );
        }
    }
}

#[test]
fn foliate_cfis_resolve_to_their_text() {
    for (fixture, cases) in [("chapter.xhtml", CHAPTER), ("kepub.xhtml", KEPUB)] {
        let chapter = compile(fixture);
        for &(needle, cfi) in cases {
            for form in [cfi.to_string(), format!("epubcfi(/6/4[ch1]!{cfi})")] {
                assert!(
                    text_at(&chapter, &form).starts_with(needle),
                    "{fixture}: {form}"
                );
            }
        }
    }
}

#[test]
fn every_text_offset_round_trips() {
    for fixture in ["chapter.xhtml", "kepub.xhtml"] {
        let chapter = compile(fixture);
        let buf = chapter.text_buffer();
        for id in chapter.iter_dfs() {
            let range = chapter.node(id).unwrap().text;
            let text = &buf[range.start as usize..range.end() as usize];
            for (i, c) in text.char_indices() {
                if c.is_whitespace() {
                    continue;
                }
                let at = range.start + i as u32;
                let cfi = chapter.text_cfi(at).unwrap();
                assert_eq!(
                    chapter.resolve_cfi(&cfi),
                    Some(SourcePosition::Text(at)),
                    "{cfi}"
                );
            }
        }
    }
}

#[test]
fn range_cfis_resolve_to_their_start() {
    let chapter = compile("chapter.xhtml");
    let cfi = "epubcfi(/6/4!/4/2[c1]/4,/2/1:0,/5:20)";
    assert!(text_at(&chapter, cfi).starts_with("bright"));
}

#[test]
fn hidden_content_snaps_to_following_text() {
    let chapter = compile_html(
        r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><body>
<p>Before.</p><p style="display:none">Secret words.</p><p>After.</p></body></html>"#,
        &[],
    );
    assert!(text_at(&chapter, "/4/4/1:3").starts_with("After."));
    assert!(text_at(&chapter, "/4/4").starts_with("After."));
}

#[test]
fn positions_past_the_end_clamp_to_the_last_text() {
    let chapter = compile("chapter.xhtml");
    let end = chapter.text_buffer().trim_end().len();
    match chapter.resolve_cfi("/4/2[c1]/99/1:0") {
        Some(SourcePosition::Text(o)) => assert!(o as usize >= end),
        other => panic!("{other:?}"),
    }
}

#[test]
fn images_resolve_to_their_element() {
    let chapter = compile_html(
        r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head/><body>
<p>Text.</p><div><img src="a.png" alt=""/></div></body></html>"#,
        &[],
    );
    let image = chapter
        .iter_dfs()
        .find(|&id| chapter.node(id).unwrap().role == boko::Role::Image)
        .unwrap();
    assert_eq!(
        chapter.source.element_path(image).as_deref(),
        Some("/4/4/2")
    );
    assert_eq!(
        chapter.resolve_cfi("/4/4/2"),
        Some(SourcePosition::Element(image))
    );
}

#[test]
fn segments_map_every_offset_like_text_cfi() {
    for fixture in ["chapter.xhtml", "kepub.xhtml"] {
        let chapter = compile(fixture);
        let buf = chapter.text_buffer();
        for seg in chapter.source.segments() {
            let path = chapter.source.path_string(seg.chunk);
            let start = seg.buf_start as usize;
            let mut src = seg.src_offset;
            for (i, c) in buf[start..start + seg.buf_len as usize].char_indices() {
                let at = (start + i) as u32;
                assert_eq!(
                    chapter.text_cfi(at),
                    Some(format!("{path}:{src}")),
                    "{fixture}"
                );
                src += c.len_utf16() as u32;
            }
        }
    }
}
