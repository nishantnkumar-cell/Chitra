//! Printing on Windows: the standard Print dialog (printer, paper, copies,
//! orientation) and GDI, which every printer driver supports. The photo is
//! drawn once per page at its place in mm, converted to printer dots.

use std::ptr::null_mut;

use image::RgbImage;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps, HALFTONE, HDC,
    HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY,
    PHYSICALWIDTH, SRCCOPY, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, VERTRES,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW, PrintDlgW,
};

use chitra::print::{Rect, Size, oriented, place};

/// Show the Print dialog over `owner` (a window handle, or 0) and print
/// `image`. Returns false when the user cancelled.
pub fn print(owner: isize, title: &str, image: &RgbImage, size: Size) -> Result<bool, String> {
    // SAFETY: PRINTDLGW is plain data; all-zero is its documented "empty"
    // state, and the size field is set before use.
    let mut pd: PRINTDLGW = unsafe { std::mem::zeroed() };
    pd.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
    pd.hwndOwner = owner as HWND;
    pd.Flags = PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE | PD_NOPAGENUMS | PD_NOSELECTION;
    pd.nCopies = 1;
    // SAFETY: `pd` is a valid, initialised PRINTDLGW that outlives the call.
    let ok = unsafe { PrintDlgW(&mut pd) } != 0;
    // SAFETY: the dialog allocated these (or left them null); we own them now
    // and free each exactly once. GlobalFree accepts null.
    let free = |pd: &PRINTDLGW| unsafe {
        GlobalFree(pd.hDevMode);
        GlobalFree(pd.hDevNames);
    };
    if !ok {
        free(&pd);
        // SAFETY: no arguments; reads the dialog's last error code.
        let err = unsafe { CommDlgExtendedError() };
        return if err == 0 {
            Ok(false)
        } else {
            Err(format!("the Print dialog failed (error {err:#x})"))
        };
    }
    let dc = pd.hDC;
    let copies = pd.nCopies.max(1);
    free(&pd);
    if dc.is_null() {
        return Err("no printer was chosen".into());
    }
    let result = draw(dc, title, image, size, copies);
    // SAFETY: `dc` came from PrintDlgW with PD_RETURNDC and is deleted once.
    unsafe { DeleteDC(dc) };
    result.map(|()| true)
}

fn draw(dc: HDC, title: &str, image: &RgbImage, size: Size, copies: u16) -> Result<(), String> {
    // SAFETY: `dc` is a valid printer DC for every call below.
    let cap = |i| unsafe { GetDeviceCaps(dc, i) } as f32;
    let (dpi_x, dpi_y) = (cap(LOGPIXELSX as i32), cap(LOGPIXELSY as i32));
    if dpi_x <= 0.0 || dpi_y <= 0.0 {
        return Err("the printer did not report its resolution".into());
    }
    let mm_x = |dots: f32| dots * 25.4 / dpi_x;
    let mm_y = |dots: f32| dots * 25.4 / dpi_y;
    let (off_x, off_y) = (cap(PHYSICALOFFSETX as i32), cap(PHYSICALOFFSETY as i32));
    let paper_w = mm_x(cap(PHYSICALWIDTH as i32));
    let paper_h = mm_y(cap(PHYSICALHEIGHT as i32));
    let printable = Rect::new(
        mm_x(off_x),
        mm_y(off_y),
        mm_x(cap(HORZRES as i32)),
        mm_y(cap(VERTRES as i32)),
    );
    // Windows' own dialog has no fill option; fill becomes fit.
    let size = if size == Size::Fill { Size::Fit } else { size };
    let p = place(
        image.width(),
        image.height(),
        size,
        paper_w,
        paper_h,
        printable,
    );
    let turned = oriented(image, &p);
    let img = turned.as_ref().unwrap_or(image);
    // Device coordinates start at the printable corner, not the paper's.
    let x = (p.dest.x / 25.4 * dpi_x - off_x).round() as i32;
    let y = (p.dest.y / 25.4 * dpi_y - off_y).round() as i32;
    let w = (p.dest.w / 25.4 * dpi_x).round() as i32;
    let h = (p.dest.h / 25.4 * dpi_y).round() as i32;

    let (iw, ih) = (img.width() as usize, img.height() as usize);
    // 24-bit DIB rows are BGR and padded to four bytes.
    let stride = (iw * 3).div_ceil(4) * 4;
    let mut bits = vec![0u8; stride * ih];
    for (row, src) in bits
        .chunks_exact_mut(stride)
        .zip(img.as_raw().chunks_exact(iw * 3))
    {
        for (d, s) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
            d.copy_from_slice(&[s[2], s[1], s[0]]);
        }
    }
    // SAFETY: plain data; every field that matters is set below.
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: iw as i32,
        biHeight: -(ih as i32), // negative: rows run top to bottom
        biPlanes: 1,
        biBitCount: 24,
        biCompression: BI_RGB,
        ..info.bmiHeader
    };

    let name: Vec<u16> = title.encode_utf16().chain([0]).collect();
    // SAFETY: plain data; `name` outlives the StartDocW call that reads it.
    let mut doc: DOCINFOW = unsafe { std::mem::zeroed() };
    doc.cbSize = std::mem::size_of::<DOCINFOW>() as i32;
    doc.lpszDocName = name.as_ptr();
    // SAFETY: `dc` is a valid printer DC and `doc` is initialised.
    if unsafe { StartDocW(dc, &doc) } <= 0 {
        return Err("the printer would not start the job".into());
    }
    for _ in 0..copies {
        // SAFETY: `dc` is valid; `bits` holds `ih` rows of `stride` bytes as
        // `info` describes, and both outlive the call.
        let drawn = unsafe {
            if StartPage(dc) <= 0 {
                0
            } else {
                SetStretchBltMode(dc, HALFTONE);
                SetBrushOrgEx(dc, 0, 0, null_mut());
                let n = StretchDIBits(
                    dc,
                    x,
                    y,
                    w,
                    h,
                    0,
                    0,
                    iw as i32,
                    ih as i32,
                    bits.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if EndPage(dc) <= 0 { 0 } else { n }
            }
        };
        if drawn <= 0 {
            // SAFETY: the job was started on this DC.
            unsafe { AbortDoc(dc) };
            return Err("the printer driver could not draw the photo".into());
        }
    }
    // SAFETY: the job was started on this DC.
    if unsafe { EndDoc(dc) } <= 0 {
        return Err("the printer would not finish the job".into());
    }
    Ok(())
}
