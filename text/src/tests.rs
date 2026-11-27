use super::*;

fn request(text: &str) -> TextRequest {
    TextRequest {
        text: text.into(),
        size: 32.0,
        weight: "regular".into(),
        italic: false,
        wrap_width: None,
        line_height: 1.25,
        font: "sans".into(),
        spacing: 0.0,
    }
}

#[test]
fn the_embedded_fonts_shape_text_beyond_ascii() {
    let text = TextSystem::new();
    for sample in ["Score 120", "Žluťoučký kůň", "Привет мир", "Αθήνα", "£€ ½ — “x”"] {
        let shaped = text.shape(&request(sample));
        assert!(!shaped.glyphs.is_empty(), "{sample}");
        assert!(
            shaped.glyphs.iter().all(|g| g.glyph != 0),
            "{sample}: a glyph is missing from the font"
        );
    }
}

#[test]
fn shaping_is_proportional_kerned_and_the_mono_family_is_not() {
    let text = TextSystem::new();
    let width = |s: &str, font: &str| {
        let mut r = request(s);
        r.font = font.into();
        text.shape(&r).size.x
    };
    assert!(width("iiii", "sans") < width("WWWW", "sans") * 0.6);
    assert!((width("iiii", "mono") - width("WWWW", "mono")).abs() < 0.01);
    let mut bold = request("Hello");
    bold.weight = "bold".into();
    assert!(text.shape(&bold).size.x > width("Hello", "sans"));
    let mut spaced = request("Hello");
    spaced.spacing = 4.0;
    let extra = text.shape(&spaced).size.x - width("Hello", "sans");
    assert!((15.0..=21.0).contains(&extra), "{extra}");
}

#[test]
fn wrapping_breaks_between_words_and_measuring_matches_drawing() {
    let text = TextSystem::new();
    let mut r = request("one two three four five six");
    let single = text.measure(&r);
    r.wrap_width = Some(single.x / 2.5);
    let wrapped = text.shape(&r);
    assert!(wrapped.line_widths.len() >= 3);
    assert!(wrapped.line_widths.iter().all(|w| *w <= single.x / 2.5 + 0.5));
    assert_eq!(text.measure(&r).y, wrapped.size.y);
    // A box exactly as wide as measured keeps one line.
    let mut fitted = request("one two three four five six");
    fitted.wrap_width = Some(single.x);
    assert_eq!(text.shape(&fitted).line_widths.len(), 1);
}

#[test]
fn shaping_is_cached() {
    let text = TextSystem::new();
    text.shape(&request("again"));
    let after = text.shaped_count();
    text.shape(&request("again"));
    assert_eq!(text.shaped_count(), after);
}

#[test]
fn raster_glyphs_land_on_whole_pixels_and_are_shared() {
    let text = TextSystem::new();
    let mut r = request("aa");
    r.size = 14.0;
    let shaped = text.shape(&r);
    let quads: Vec<_> = shaped
        .glyphs
        .iter()
        .filter_map(|g| text.raster_glyph(&shaped, g, [10.0, 20.0], 1.0))
        .collect();
    assert_eq!(quads.len(), 2);
    for quad in &quads {
        assert_eq!(quad.rect[0].fract(), 0.0);
        assert_eq!(quad.rect[1].fract(), 0.0);
        assert_eq!(quad.rect[2], quad.uv[2] - quad.uv[0]);
    }
    assert!(text.atlas().len() <= 2, "one image per sub-pixel position");
}

/// The distance field of a glyph puts the edge where the outline is: the
/// median of its channels agrees with the hinted raster's inside and outside,
/// and one image serves every size.
#[test]
fn distance_fields_agree_with_the_rasteriser() {
    let text = TextSystem::new();
    let mut r = request("AMkg4&@");
    r.size = msdf::EM_TEXELS;
    let shaped = text.shape(&r);
    for glyph in &shaped.glyphs {
        let field = text.field_glyph(&shaped, glyph, [0.0, 0.0], 1.0).unwrap();
        let raster = text.raster_glyph(&shaped, glyph, [0.0, 0.0], 1.0).unwrap();
        let atlas = text.atlas();
        let texel = |x: u32, y: u32| {
            let at = ((y * atlas::ATLAS_SIZE + x) * 4) as usize;
            &atlas.texels()[at..at + 4]
        };
        let (mut wrong, mut compared) = (0u32, 0u32);
        for ry in 0..raster.rect[3] as u32 {
            for rx in 0..raster.rect[2] as u32 {
                let coverage = texel(raster.uv[0] as u32 + rx, raster.uv[1] as u32 + ry)[0];
                if (8..248).contains(&coverage) {
                    continue; // an edge pixel
                }
                // The same point in the field.
                let px = raster.rect[0] + rx as f32 + 0.5 - field.rect[0];
                let py = raster.rect[1] + ry as f32 + 0.5 - field.rect[1];
                let (w, h) = (field.uv[2] - field.uv[0], field.uv[3] - field.uv[1]);
                let (fx, fy) = (px * w / field.rect[2], py * h / field.rect[3]);
                if fx < 0.0 || fy < 0.0 || fx >= w || fy >= h {
                    continue;
                }
                let t = texel(field.uv[0] as u32 + fx as u32, field.uv[1] as u32 + fy as u32);
                let median = t[0].max(t[1]).min(t[0].min(t[1]).max(t[2]));
                // Only texels clearly on one side: the raster is hinted
                // vertically, so its edges sit up to a pixel from the outline.
                if median.abs_diff(128) < 40 {
                    continue;
                }
                compared += 1;
                wrong += u32::from((median > 128) != (coverage > 128));
            }
        }
        assert!(compared > 50, "glyph {}: {compared}", glyph.glyph);
        assert!(
            wrong * 50 < compared,
            "glyph {}: {wrong} of {compared} texels disagree",
            glyph.glyph
        );
    }
}
