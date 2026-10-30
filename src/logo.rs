//! Shared vector artwork from the original Benga logo.
use crate::theme::mix;
use anyhow::{ensure, Context};
use quick_xml::{events::Event, Reader};
use vello::{
    kurbo::{Affine, BezPath, Rect, Shape},
    peniko::{Color, Fill},
    Scene,
};

const LOGO: &str = include_str!("../assets/benga-logo.svg");

struct LogoPath {
    path: BezPath,
    transform: Affine,
    outline: bool,
}

pub(crate) struct Logo {
    paths: Vec<LogoPath>,
    bounds: Rect,
}

impl Logo {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let paths = logo_paths()?;
        let bounds = paths
            .iter()
            .map(|path| (path.transform * &path.path).bounding_box())
            .reduce(|bounds, path| bounds.union(path))
            .context("The bundled Benga logo must contain visible paths")?;
        ensure!(
            bounds.width() > 0.0 && bounds.height() > 0.0,
            "The bundled logo is empty"
        );
        Ok(Self { paths, bounds })
    }

    pub(crate) fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Fit the original artwork into a local rectangle, preserving its aspect ratio.
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        area: Rect,
        outer: Affine,
        background: Color,
        ink: Color,
    ) {
        let scale = (area.width() / self.bounds.width()).min(area.height() / self.bounds.height());
        let transform = outer
            * Affine::translate((
                area.center().x - self.bounds.width() * scale * 0.5,
                area.center().y - self.bounds.height() * scale * 0.5,
            ))
            * Affine::scale(scale)
            * Affine::translate((-self.bounds.x0, -self.bounds.y0));
        for path in &self.paths {
            let color = if path.outline {
                mix(background, ink, 0.28).with_alpha(ink.components[3])
            } else {
                ink
            };
            scene.fill(
                Fill::NonZero,
                transform * path.transform,
                color,
                None,
                &path.path,
            );
        }
    }
}

/// Decode the path-and-matrix subset used by the bundled Inkscape artwork.
/// This reads the source SVG directly, keeping the titlebar and splash in sync;
/// fonts, editor metadata and the unused Inkscape path effect are irrelevant.
fn logo_paths() -> anyhow::Result<Vec<LogoPath>> {
    let mut reader = Reader::from_str(LOGO);
    let mut transforms = vec![Affine::IDENTITY];
    let mut paths = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(tag) if tag.name().as_ref() == b"g" => {
                let parent = *transforms.last().expect("root transform is retained");
                let local = tag
                    .try_get_attribute("transform")?
                    .map(|attribute| matrix(&attribute.unescape_value()?))
                    .transpose()?
                    .unwrap_or(Affine::IDENTITY);
                transforms.push(parent * local);
            }
            Event::End(tag) if tag.name().as_ref() == b"g" => {
                ensure!(transforms.len() > 1, "Unbalanced logo groups");
                transforms.pop();
            }
            Event::Start(tag) | Event::Empty(tag) if tag.name().as_ref() == b"path" => {
                let Some(data) = tag.try_get_attribute("d")? else {
                    continue;
                };
                let path = BezPath::from_svg(&data.unescape_value()?)?;
                if path.bounding_box().area() == 0.0 {
                    continue;
                }
                let local = tag
                    .try_get_attribute("transform")?
                    .map(|attribute| matrix(&attribute.unescape_value()?))
                    .transpose()?
                    .unwrap_or(Affine::IDENTITY);
                let outline = tag
                    .try_get_attribute("style")?
                    .map(|style| {
                        style
                            .unescape_value()
                            .map(|style| style.contains("fill:#b7b7c8"))
                    })
                    .transpose()?
                    .unwrap_or(false);
                paths.push(LogoPath {
                    path,
                    transform: *transforms.last().expect("root transform is retained") * local,
                    outline,
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(paths)
}

fn matrix(value: &str) -> anyhow::Result<Affine> {
    let components = value
        .trim()
        .strip_prefix("matrix(")
        .and_then(|value| value.strip_suffix(')'))
        .context("The bundled logo uses SVG matrix transforms")?
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<f64>, _>>()?;
    ensure!(
        components.len() == 6 && components.iter().all(|v| v.is_finite()),
        "Invalid logo matrix"
    );
    Ok(Affine::new(
        components.try_into().expect("six components checked"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_logo_keeps_its_outline_letters_and_group_transform() -> anyhow::Result<()> {
        let paths = logo_paths()?;
        assert_eq!(paths.iter().filter(|path| path.outline).count(), 1);
        assert_eq!(paths.len(), 6);
        let mut bounds = paths[0].path.bounding_box();
        for path in &paths[1..] {
            bounds = bounds.union((path.transform * &path.path).bounding_box());
        }
        assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0);
        assert!(bounds.x1 <= 512.0 && bounds.y1 <= 256.0);
        Ok(())
    }
}
