//! Raster images in the display list: the rasterizer's `Item::Image` (scaling,
//! alpha, clips), and the builder's `<img>` and `background-image` items through
//! the whole pipeline. Every pixel expectation is exact.

use std::collections::BTreeMap;
use std::sync::Arc;

use px_layout::geom::px;
use px_paint::raster::Canvas;
use px_paint::{
    Color, Corner, CornerShape, DisplayList, ImageData, ImageFilter, ImageSet, Item, ListError,
    Rect, RoundedRect, rasterize,
};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];

fn image(w: u32, h: u32, px_: &[[u8; 4]]) -> Arc<ImageData> {
    assert_eq!(px_.len(), (w * h) as usize);
    Arc::new(ImageData {
        width: w,
        height: h,
        rgba: px_.iter().flatten().copied().collect(),
    })
}

/// 2x2: red green / blue yellow.
fn quad() -> Arc<ImageData> {
    image(2, 2, &[RED, GREEN, BLUE, YELLOW])
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect {
        x: px(x),
        y: px(y),
        width: px(w),
        height: px(h),
    }
}

fn list_with(img: &Arc<ImageData>, items: impl FnOnce(u32) -> Vec<Item>) -> DisplayList {
    let mut l = DisplayList::new();
    let idx = l.add_image(img).expect("room");
    l.items = items(idx);
    l
}

fn draw(l: &DisplayList, w: u32, h: u32) -> Canvas {
    rasterize(l, w, h).expect("rasterized")
}

#[test]
fn nearest_doubles_a_2x2_into_4x4_blocks() {
    let l = list_with(&quad(), |image| {
        vec![Item::Image {
            rect: rect(0, 0, 4, 4),
            image,
            filter: ImageFilter::Nearest,
        }]
    });
    let cv = draw(&l, 4, 4);
    for y in 0..4 {
        for x in 0..4 {
            let want = match (x / 2, y / 2) {
                (0, 0) => RED,
                (1, 0) => GREEN,
                (0, 1) => BLUE,
                _ => YELLOW,
            };
            assert_eq!(cv.pixel(x, y), Some(want), "{x},{y}");
        }
    }
}

#[test]
fn smooth_at_one_to_one_is_an_exact_copy_and_offsets_snap() {
    let l = list_with(&quad(), |image| {
        vec![Item::Image {
            rect: rect(1, 1, 2, 2),
            image,
            filter: ImageFilter::Smooth,
        }]
    });
    let cv = draw(&l, 4, 4);
    assert_eq!(cv.pixel(0, 0), Some(WHITE));
    assert_eq!(cv.pixel(1, 1), Some(RED));
    assert_eq!(cv.pixel(2, 1), Some(GREEN));
    assert_eq!(cv.pixel(1, 2), Some(BLUE));
    assert_eq!(cv.pixel(2, 2), Some(YELLOW));
    assert_eq!(cv.pixel(3, 3), Some(WHITE));
}

#[test]
fn smooth_magnification_is_bilinear_with_edge_clamp() {
    // 2x1 black/white stretched to 4: weights 0, 1/4, 3/4, 1.
    let img = image(2, 1, &[[0, 0, 0, 255], [200, 200, 200, 255]]);
    let l = list_with(&img, |image| {
        vec![Item::Image {
            rect: rect(0, 0, 4, 1),
            image,
            filter: ImageFilter::Smooth,
        }]
    });
    let cv = draw(&l, 4, 1);
    let reds: Vec<u8> = (0..4).map(|x| cv.pixel(x, 0).expect("px")[0]).collect();
    assert_eq!(reds, vec![0, 50, 150, 200]);
}

#[test]
fn smooth_minification_is_a_box_average() {
    let img = image(
        4,
        1,
        &[
            [0, 0, 0, 255],
            [100, 100, 100, 255],
            [200, 200, 200, 255],
            [40, 40, 40, 255],
        ],
    );
    let l = list_with(&img, |image| {
        vec![Item::Image {
            rect: rect(0, 0, 2, 1),
            image,
            filter: ImageFilter::Smooth,
        }]
    });
    let cv = draw(&l, 2, 1);
    assert_eq!(cv.pixel(0, 0), Some([50, 50, 50, 255]));
    assert_eq!(cv.pixel(1, 0), Some([120, 120, 120, 255]));
}

#[test]
fn alpha_composites_over_what_is_under_it() {
    // Half-transparent red over white: (255*128 + 255*127 + 127)/255-ish; the
    // canvas rule is (s*a + d*(255-a) + 127) / 255.
    let img = image(1, 1, &[[255, 0, 0, 128]]);
    let l = list_with(&img, |image| {
        vec![
            Item::FillRect {
                rect: rect(0, 0, 1, 1),
                color: Color {
                    r: 0,
                    g: 0,
                    b: 255,
                    a: 255,
                },
            },
            Item::Image {
                rect: rect(0, 0, 2, 1),
                image,
                filter: ImageFilter::Nearest,
            },
        ]
    });
    let cv = draw(&l, 2, 1);
    let mix = |s: u32, d: u32| ((s * 128 + d * 127 + 127) / 255) as u8;
    assert_eq!(cv.pixel(0, 0), Some([mix(255, 0), 0, mix(0, 255), 255]));
    assert_eq!(
        cv.pixel(1, 0),
        Some([mix(255, 255), mix(0, 255), mix(0, 255), 255])
    );
    // A fully transparent pixel changes nothing.
    let clear = image(1, 1, &[[9, 9, 9, 0]]);
    let l = list_with(&clear, |image| {
        vec![Item::Image {
            rect: rect(0, 0, 1, 1),
            image,
            filter: ImageFilter::Nearest,
        }]
    });
    assert_eq!(draw(&l, 1, 1).pixel(0, 0), Some(WHITE));
}

#[test]
fn a_plain_clip_cuts_the_image_and_scaling_ignores_the_cut() {
    // The 2x2 stretched over 8x8 at (0,0), clipped to x in 2..6 y in 2..6: the
    // visible pixels are those of the unclipped drawing.
    let l = list_with(&quad(), |image| {
        vec![
            Item::PushClip {
                rect: rect(2, 2, 4, 4),
            },
            Item::Image {
                rect: rect(0, 0, 8, 8),
                image,
                filter: ImageFilter::Nearest,
            },
            Item::PopClip,
        ]
    });
    let cv = draw(&l, 8, 8);
    assert_eq!(cv.pixel(1, 1), Some(WHITE));
    assert_eq!(cv.pixel(2, 2), Some(RED));
    assert_eq!(cv.pixel(5, 2), Some(GREEN));
    assert_eq!(cv.pixel(2, 5), Some(BLUE));
    assert_eq!(cv.pixel(5, 5), Some(YELLOW));
    assert_eq!(cv.pixel(6, 6), Some(WHITE));
}

fn round_shape(r: i32, x: i32, y: i32, w: i32, h: i32) -> RoundedRect {
    let c = Corner {
        rx: px(r),
        ry: px(r),
        shape: CornerShape::ROUND,
    };
    RoundedRect::new(rect(x, y, w, h), [c; 4])
}

#[test]
fn a_rounded_clip_rounds_the_image() {
    let solid = image(1, 1, &[RED]);
    let l = list_with(&solid, |image| {
        vec![
            Item::PushRoundedClip {
                shape: round_shape(8, 0, 0, 16, 16),
            },
            Item::Image {
                rect: rect(0, 0, 16, 16),
                image,
                filter: ImageFilter::Nearest,
            },
            Item::PopClip,
        ]
    });
    let cv = draw(&l, 16, 16);
    // The corner pixel is outside the circle; the centre and mid-edges are in.
    assert_eq!(cv.pixel(0, 0), Some(WHITE));
    assert_eq!(cv.pixel(15, 15), Some(WHITE));
    assert_eq!(cv.pixel(8, 8), Some(RED));
    assert_eq!(cv.pixel(8, 0), Some(RED));
    assert_eq!(cv.pixel(0, 8), Some(RED));
    // The edge of the curve is anti-aliased: some pixel is neither.
    let edge = cv.pixel(1, 2).expect("px");
    assert!(edge != WHITE && edge != RED, "{edge:?}");
}

#[test]
fn the_table_is_validated_and_deduplicated() {
    let img = quad();
    let mut l = DisplayList::new();
    assert_eq!(l.add_image(&img), Some(0));
    assert_eq!(l.add_image(&img), Some(0), "the same Arc is one entry");
    assert_eq!(
        l.add_image(&quad()),
        Some(1),
        "an equal image elsewhere is not"
    );
    l.items.push(Item::Image {
        rect: rect(0, 0, 1, 1),
        image: 7,
        filter: ImageFilter::Nearest,
    });
    assert_eq!(l.validate(), Err(ListError::ImageOutOfBounds));
    assert!(rasterize(&l, 2, 2).is_err());
    let mut bad = DisplayList::new();
    bad.images.push(Arc::new(ImageData {
        width: 2,
        height: 2,
        rgba: vec![0; 3],
    }));
    assert_eq!(bad.validate(), Err(ListError::BadImage));
}

#[test]
fn a_huge_destination_only_pays_for_what_is_visible() {
    // 1x1 stretched over 100000 x 100000 px, viewport 4x4: instant.
    let img = image(1, 1, &[GREEN]);
    let l = list_with(&img, |image| {
        vec![Item::Image {
            rect: rect(-50_000, -50_000, 100_000, 100_000),
            image,
            filter: ImageFilter::Smooth,
        }]
    });
    let cv = draw(&l, 4, 4);
    assert_eq!(cv.pixel(0, 0), Some(GREEN));
    assert_eq!(cv.pixel(3, 3), Some(GREEN));
}

// -- The builder, through layout ----------------------------------------------

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
                    head, style, title { display: none }";

fn node_by_id(arena: &px_dom::Arena, id: &str) -> px_dom::NodeId {
    let document = arena.document();
    core::iter::once(document)
        .chain(arena.descendants(document))
        .find(|n| {
            arena
                .get(*n)
                .and_then(|node| node.attrs())
                .is_some_and(|a| a.iter().any(|a| &*a.name.local == "id" && &*a.value == id))
        })
        .expect("an element with that id")
}

/// Lay out and paint `body` at `w` x `h`; `elements` are the pictures of `<img>`
/// ids, `urls` those of `background-image` URLs (as the stylesheet resolves
/// them against `https://example.invalid/a.css`).
fn page(
    body: &str,
    css: &str,
    elements: &[(&str, Arc<ImageData>)],
    urls: &[(&str, Arc<ImageData>)],
    w: u32,
    h: u32,
) -> (DisplayList, Canvas) {
    let dom = px_dom::parse(&format!("<html><body>{body}</body></html>"));
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(w as f32, h as f32, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("a root element");
    let mut set = ImageSet::default();
    let mut sizes = BTreeMap::new();
    for (id, img) in elements {
        let n = node_by_id(&arena, id);
        sizes.insert(n, (img.width, img.height));
        set.elements.insert(n, img.clone());
    }
    for (u, img) in urls {
        set.urls.insert((*u).to_string(), img.clone());
    }
    let tree =
        px_layout::replaced::layout_document_with_images(&arena, &root, px(w as i32), &sizes)
            .expect("laid out");
    let list = px_paint::build_with_images(&arena, &root, &tree, px(w as i32), px(h as i32), &set);
    assert!(list.clips_balanced());
    let cv = rasterize(&list, w, h).expect("rasterized");
    (list, cv)
}

#[test]
fn an_img_paints_at_its_intrinsic_size_with_a_crisp_pixelated_scale() {
    let (_, cv) = page(
        "<img id=i style='image-rendering:pixelated; width:8px'>",
        "img { display: block }",
        &[("i", quad())],
        &[],
        10,
        10,
    );
    // 2x2 -> 8x8 (ratio keeps the height): 4px blocks.
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(3, 3), Some(RED));
    assert_eq!(cv.pixel(4, 0), Some(GREEN));
    assert_eq!(cv.pixel(0, 4), Some(BLUE));
    assert_eq!(cv.pixel(7, 7), Some(YELLOW));
    assert_eq!(cv.pixel(8, 0), Some(WHITE));
}

#[test]
fn object_fit_contain_cover_none_and_position() {
    let wide = image(4, 2, &[RED, RED, GREEN, GREEN, RED, RED, GREEN, GREEN]);
    let css = "img { display: block; width: 8px; height: 8px; image-rendering: pixelated }";
    // contain: 8x4 in the middle (object-position 50% 50%): rows 2..6.
    let (_, cv) = page(
        "<img id=i style='object-fit:contain'>",
        css,
        &[("i", wide.clone())],
        &[],
        8,
        8,
    );
    assert_eq!(cv.pixel(0, 1), Some(WHITE));
    assert_eq!(cv.pixel(0, 2), Some(RED));
    assert_eq!(cv.pixel(7, 5), Some(GREEN));
    assert_eq!(cv.pixel(0, 6), Some(WHITE));
    // cover: 16x8 centred, so x in -4..12; the box shows source x 1..3.
    let (_, cv) = page(
        "<img id=i style='object-fit:cover'>",
        css,
        &[("i", wide.clone())],
        &[],
        8,
        8,
    );
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(7, 7), Some(GREEN));
    // none, object-position: left top: 4x2 at the corner, nothing else.
    let (_, cv) = page(
        "<img id=i style='object-fit:none; object-position:0 0'>",
        css,
        &[("i", wide.clone())],
        &[],
        8,
        8,
    );
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(3, 1), Some(GREEN));
    assert_eq!(cv.pixel(4, 0), Some(WHITE));
    assert_eq!(cv.pixel(0, 2), Some(WHITE));
    // none, right bottom.
    let (_, cv) = page(
        "<img id=i style='object-fit:none; object-position:100% 100%'>",
        css,
        &[("i", wide)],
        &[],
        8,
        8,
    );
    assert_eq!(cv.pixel(3, 5), Some(WHITE));
    assert_eq!(cv.pixel(4, 6), Some(RED));
    assert_eq!(cv.pixel(7, 7), Some(GREEN));
}

#[test]
fn an_img_honours_border_radius_and_padding() {
    let solid = image(1, 1, &[RED]);
    let (_, cv) = page(
        "<img id=i style='border-radius:8px; padding:2px; width:12px; height:12px'>",
        "img { display: block }",
        &[("i", solid)],
        &[],
        16,
        16,
    );
    // The border box is 16x16 with padding 2: the picture starts at 2,2.
    assert_eq!(cv.pixel(1, 8), Some(WHITE));
    assert_eq!(cv.pixel(8, 8), Some(RED));
    // The padded corner is rounded away.
    assert_eq!(cv.pixel(2, 2), Some(WHITE));
}

#[test]
fn background_image_sizes_positions_and_repeats() {
    let tile = quad();
    let css = |extra: &str| {
        format!(
            "div {{ width: 8px; height: 8px; background-image: url(t.png); \
             image-rendering: pixelated; {extra} }}"
        )
    };
    let urls = [("https://example.invalid/t.png", tile)];
    // Intrinsic size, repeating: the 2x2 tiles across 8x8.
    let (_, cv) = page("<div></div>", &css(""), &[], &urls, 8, 8);
    for (x, y, want) in [
        (0, 0, RED),
        (1, 0, GREEN),
        (0, 1, BLUE),
        (1, 1, YELLOW),
        (2, 0, RED),
        (7, 7, YELLOW),
        (6, 6, RED),
    ] {
        assert_eq!(cv.pixel(x, y), Some(want), "{x},{y}");
    }
    // no-repeat at the right bottom, 4x4.
    let (_, cv) = page(
        "<div></div>",
        &css(
            "background-repeat: no-repeat; background-size: 4px 4px; background-position: right bottom",
        ),
        &[],
        &urls,
        8,
        8,
    );
    assert_eq!(cv.pixel(3, 3), Some(WHITE));
    assert_eq!(cv.pixel(4, 4), Some(RED));
    assert_eq!(cv.pixel(7, 7), Some(YELLOW));
    // cover and contain on a non-square box.
    let (_, cv) = page(
        "<div style='width:8px;height:4px'></div>",
        &css("background-repeat: no-repeat; background-size: contain"),
        &[],
        &urls,
        8,
        8,
    );
    // contain: 4x4 at the left (position 0% 0%).
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(3, 3), Some(YELLOW));
    assert_eq!(cv.pixel(4, 0), Some(WHITE));
    let (_, cv) = page(
        "<div style='width:8px;height:4px'></div>",
        &css("background-repeat: no-repeat; background-size: cover"),
        &[],
        &urls,
        8,
        8,
    );
    // cover: 8x8 from the top; the box (8x4) shows its upper half.
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(7, 3), Some(GREEN));
    assert_eq!(cv.pixel(0, 4), Some(WHITE));
}

#[test]
fn background_repeat_space_and_round() {
    let tile = image(2, 2, &[RED, RED, RED, RED]);
    let urls = [("https://example.invalid/t.png", tile)];
    let css = |extra: &str| {
        format!(
            "div {{ width: 11px; height: 2px; background-image: url(t.png); \
             background-repeat: {extra}; image-rendering: pixelated }}"
        )
    };
    // space: 5 whole tiles of 2 in 11 px, gaps of 0.25: first at 0, last ends
    // at 11; every tile snaps independently, so the first and last columns are
    // painted.
    let (_, cv) = page("<div></div>", &css("space no-repeat"), &[], &urls, 12, 2);
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(10, 0), Some(RED));
    assert_eq!(cv.pixel(11, 0), Some(WHITE));
    // round: 11/2 -> 6 tiles of 1.833px (floor to Au), covering the width.
    let (_, cv) = page("<div></div>", &css("round no-repeat"), &[], &urls, 12, 2);
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(10, 0), Some(RED));
}

#[test]
fn background_images_clip_to_the_border_radius_and_sit_under_the_border() {
    let solid = image(1, 1, &[RED]);
    let urls = [("https://example.invalid/t.png", solid)];
    let (_, cv) = page(
        "<div></div>",
        "div { width: 16px; height: 16px; border-radius: 8px; background-image: url(t.png); \
         background-size: 100% 100%; border: 2px solid #00f; background-clip: border-box }",
        &[],
        &urls,
        20,
        20,
    );
    // A 20x20 border box (16 + 2*2), corner radius 8.
    assert_eq!(cv.pixel(0, 0), Some(WHITE));
    // The border is drawn over the image.
    assert_eq!(cv.pixel(10, 0), Some(BLUE));
    assert_eq!(cv.pixel(10, 10), Some(RED));
}

#[test]
fn the_propagated_background_image_covers_the_canvas() {
    let tile = image(2, 2, &[RED, RED, RED, RED]);
    let urls = [("https://example.invalid/t.png", tile)];
    let (_, cv) = page(
        "<div style='height:1px'></div>",
        "body { background-image: url(t.png) }",
        &[],
        &urls,
        6,
        6,
    );
    // body is 1px tall but its image fills the whole canvas.
    assert_eq!(cv.pixel(5, 5), Some(RED));
    assert_eq!(cv.pixel(0, 5), Some(RED));
}

#[test]
fn an_empty_image_set_builds_the_same_list_as_before() {
    let dom = px_dom::parse("<html><body><div style='height:3px;background:#0f0'>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(8.0, 8.0, quirks);
    engine.add_author_stylesheet(BASE, "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("root");
    let tree = px_layout::block::layout_document(&arena, &root, px(8)).expect("tree");
    let a = px_paint::build_with_viewport(&arena, &root, &tree, px(8), px(8));
    let b = px_paint::build_with_images(&arena, &root, &tree, px(8), px(8), &ImageSet::default());
    assert_eq!(a, b);
    assert!(a.images.is_empty());
}
