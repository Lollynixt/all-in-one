//! Document fonts. DejaVu is bundled so bold and serif look the same on every OS; common system fonts are added when found.
use egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

#[derive(Clone)]
pub struct DocFont {
    pub name: String,
    pub regular: FontFamily,
    pub bold: FontFamily,
}

const SYSTEM: &[(&str, &[&str], &[&str])] = &[
    ("Georgia", &["/System/Library/Fonts/Supplemental/Georgia.ttf", "C:\\Windows\\Fonts\\georgia.ttf"], &["/System/Library/Fonts/Supplemental/Georgia Bold.ttf", "C:\\Windows\\Fonts\\georgiab.ttf"]),
    ("Arial", &["/System/Library/Fonts/Supplemental/Arial.ttf", "C:\\Windows\\Fonts\\arial.ttf"], &["/System/Library/Fonts/Supplemental/Arial Bold.ttf", "C:\\Windows\\Fonts\\arialbd.ttf"]),
    ("Times New Roman", &["/System/Library/Fonts/Supplemental/Times New Roman.ttf", "C:\\Windows\\Fonts\\times.ttf"], &["/System/Library/Fonts/Supplemental/Times New Roman Bold.ttf", "C:\\Windows\\Fonts\\timesbd.ttf"]),
    ("Verdana", &["/System/Library/Fonts/Supplemental/Verdana.ttf", "C:\\Windows\\Fonts\\verdana.ttf"], &["/System/Library/Fonts/Supplemental/Verdana Bold.ttf", "C:\\Windows\\Fonts\\verdanab.ttf"]),
    ("Courier New", &["/System/Library/Fonts/Supplemental/Courier New.ttf", "C:\\Windows\\Fonts\\cour.ttf"], &["/System/Library/Fonts/Supplemental/Courier New Bold.ttf", "C:\\Windows\\Fonts\\courbd.ttf"]),
];

fn first_readable(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

fn register(defs: &mut FontDefinitions, key: &str, data: FontData) -> FontFamily {
    defs.font_data.insert(key.to_string(), Arc::new(data));
    // Fall back to the default fonts for symbols and emoji the document font lacks.
    let mut chain = vec![key.to_string()];
    chain.extend(defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    let fam = FontFamily::Name(key.into());
    defs.families.insert(fam.clone(), chain);
    fam
}

pub fn install(ctx: &egui::Context) -> Vec<DocFont> {
    let mut defs = FontDefinitions::default();
    let mut out = vec![];

    let bundled: [(&str, &'static [u8], &'static [u8]); 3] = [
        ("Sans", include_bytes!("../assets/fonts/DejaVuSans.ttf"), include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf")),
        ("Serif", include_bytes!("../assets/fonts/DejaVuSerif.ttf"), include_bytes!("../assets/fonts/DejaVuSerif-Bold.ttf")),
        ("Mono", include_bytes!("../assets/fonts/DejaVuSansMono.ttf"), include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf")),
    ];
    for (name, reg, bold) in bundled {
        let regular = register(&mut defs, name, FontData::from_static(reg));
        let bold = register(&mut defs, &format!("{name} Bold"), FontData::from_static(bold));
        out.push(DocFont { name: name.into(), regular, bold });
    }
    for (name, reg, bold) in SYSTEM {
        let Some(reg_bytes) = first_readable(reg) else { continue };
        let regular = register(&mut defs, name, FontData::from_owned(reg_bytes.clone()));
        let bold_bytes = first_readable(bold).unwrap_or(reg_bytes);
        let bold = register(&mut defs, &format!("{name} Bold"), FontData::from_owned(bold_bytes));
        out.push(DocFont { name: (*name).into(), regular, bold });
    }
    ctx.set_fonts(defs);
    out
}
