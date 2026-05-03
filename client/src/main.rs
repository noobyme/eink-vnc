#[macro_use]
extern crate log;
extern crate byteorder;
extern crate flate2;

mod device;
mod framebuffer;
#[macro_use]
mod geom;
mod color;
mod input;
mod security;
mod settings;
mod vnc;
mod gesture;
mod unit;
// mod gesture;

use crate::framebuffer::{Framebuffer, KoboFramebuffer1, KoboFramebuffer2, Pixmap, UpdateMode};
use crate::framebuffer::transform::transform_dither_g2;
use crate::geom::{Rectangle,Dir};
use crate::vnc::{client, Client, Encoding, Rect};
use clap::{value_t, App, Arg};
use log::{debug, error, info};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use vnc::PixelFormat;
use input::{raw_events, device_events, usb_events, display_rotate_event, button_scheme_event,
            DeviceEvent, FingerStatus, ButtonCode, ButtonStatus};

use anyhow::{Context as ResultExt, Error};

use crate::device::CURRENT_DEVICE;
use std::fs::File;
use std::io::Read;
use std::mem;
use std::slice;
//use std::thread;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use crate::color::Color;
use crate::gesture::*;

const FB_DEVICE: &str = "/dev/fb0";

const TOUCH_INPUTS: [&str; 5] = ["/dev/input/by-path/platform-2-0010-event",
    "/dev/input/by-path/platform-1-0038-event",
    "/dev/input/by-path/platform-1-0010-event",
    "/dev/input/by-path/platform-0-0010-event",
    "/dev/input/event1"];

const BUTTON_INPUTS: [&str; 4] = ["/dev/input/by-path/platform-gpio-keys-event",
    "/dev/input/by-path/platform-ntx_event0-event",
    "/dev/input/by-path/platform-mxckpd-event",
    "/dev/input/event0"];
const POWER_INPUTS: [&str; 3] = ["/dev/input/by-path/platform-bd71828-pwrkey.6.auto-event",
    "/dev/input/by-path/platform-bd71828-pwrkey.4.auto-event",
    "/dev/input/by-path/platform-bd71828-pwrkey-event"];

#[repr(align(256))]
pub struct PostProcBin {
    data: [u8; 256],
}


fn main() -> Result<(), Error> {
    env_logger::init();

    let matches = App::new("einkvnc")
        .about("VNC client")
        .arg(
            Arg::with_name("HOST")
                .help("server hostname or IP")
                .required(true)
                .takes_value(true)
                .index(1),
        )
        .arg(
            Arg::with_name("PORT")
                .help("server port (default: 5900)")
                .takes_value(true)
                .index(2),

        )
        .arg(
            Arg::with_name("USERNAME")
                .help("server username")
                .long("username")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("PASSWORD")
                .help("server password")
                .long("password")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("EXCLUSIVE")
                .help("request a non-shared session")
                .long("exclusive"),
        )
        .arg(
            Arg::with_name("CONTRAST")
                .help("apply a post processing contrast filter")
                .long("contrast")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("GRAYPOINT")
                .help("the gray point of the post processing contrast filter")
                .long("graypoint")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("WHITECUTOFF")
                .help("apply a post processing filter to turn colors greater than the specified value to white (255)")
                .long("whitecutoff")
                .takes_value(true),
        )
        .arg(
        Arg::with_name("ROTATE")
            .help("rotation (1-4), tested on a Clara HD, try at own risk")
            .long("rotate")
            .takes_value(true),
        )
        .arg(
            Arg::with_name("SCALE")
                .help("fit to height or width")
                .long("scale"),
        )
        .arg(
            Arg::with_name("LONG_TAP")
                .help("long tap to send right click, for pc servers. not necessary for touchscreen servers or linux servers")
                .long("long_tap"),
        )
        .arg(
            Arg::with_name("FULL_UPDATE")
                .help("Choose 1=Fast 2=Fastmono 3=Gui 4=Partial 5=Full")
                .long("full_update")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("PARTIAL_UPDATE")
                .help("Choose 1=Fast 2=Fastmono 3=Gui 4=Partial 5=Full")
                .long("partial_update")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("SET_DITHER")
                .help("true or false")
                .long("set_dither")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("SET_MONOCHROME")
                .help("true or false")
                .long("set_monochrome")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("FULL_REFRESH")
                .help("Choose how often to full refresh")
                .long("full_refresh")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("FPS")
                .help("Choose how often to request update")
                .long("fps")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("BPP")
                .help("Choose colour bpp")
                .long("bpp")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("BLUE_NOISE")
                .help("Blue noise dithering for 1bit output")
                .long("blue_noise")
        )
        .arg(
            Arg::with_name("PAN")
                .help("Swipe to pan instead of swipe to drag")
                .long("pan")
        )
        .arg(
            Arg::with_name("RED_SHIFT")
                .help("If have colour inaccuracy, switch red shift and blue shift")
                .long("red_shift")
        )
            .get_matches();

    let host = matches.value_of("HOST").unwrap();
    let port = value_t!(matches.value_of("PORT"), u16).unwrap_or(5900);
    let username = matches.value_of("USERNAME");
    let password = matches.value_of("PASSWORD");
    let contrast_exp = value_t!(matches.value_of("CONTRAST"), f32).unwrap_or(1.0);
    let contrast_gray_point = value_t!(matches.value_of("GRAYPOINT"), f32).unwrap_or(224.0);
    let white_cutoff = value_t!(matches.value_of("WHITECUTOFF"), u8).unwrap_or(255);
    let exclusive = matches.is_present("EXCLUSIVE");
    let rotate = value_t!(matches.value_of("ROTATE"), i8).unwrap_or(CURRENT_DEVICE.startup_rotation());
    let scale = matches.is_present("SCALE");
    let long_tap = matches.is_present("LONG_TAP");
    let full_update = value_t!(matches.value_of("FULL_UPDATE"), i8).unwrap_or(5);
    let partial_update = value_t!(matches.value_of("PARTIAL_UPDATE"), i8).unwrap_or(4);
    let set_dither = value_t!(matches.value_of("SET_DITHER"), bool).unwrap_or(false);
    let set_monochrome = value_t!(matches.value_of("SET_MONOCHROME"), bool).unwrap_or(false);
    let refresh = value_t!(matches.value_of("FULL_REFRESH"), u32).unwrap_or(500);
    let fps = value_t!(matches.value_of("FPS"), f32).unwrap_or(30.0);
    let bits_format = value_t!(matches.value_of("BPP"), u8).unwrap_or(8);
    let blue_noise = matches.is_present("BLUE_NOISE");
    let is_swipe = matches.is_present("PAN");
    let red_shift = matches.is_present("RED_SHIFT");


    info!("connecting to {}:{}", host, port);
    let stream = match std::net::TcpStream::connect((host, port)) {
        Ok(stream) => stream,
        Err(error) => {
            error!("cannot connect to {}:{}: {}", host, port, error);
            std::process::exit(1)
        }
    };

    let mut vnc = match Client::from_tcp_stream(stream, !exclusive, |methods| {
        debug!("available authentication methods: {:?}", methods);
        for method in methods {
            match method {
                client::AuthMethod::None => return Some(client::AuthChoice::None),
                client::AuthMethod::Password => {
                    return match password {
                        None => None,
                        Some(ref password) => {
                            let mut key = [0; 8];
                            for (i, byte) in password.bytes().enumerate() {
                                if i == 8 {
                                    break;
                                }
                                key[i] = byte
                            }
                            Some(client::AuthChoice::Password(key))
                        }
                    }
                }
                client::AuthMethod::AppleRemoteDesktop => match (username, password) {
                    (Some(username), Some(password)) => {
                        return Some(client::AuthChoice::AppleRemoteDesktop(
                            username.to_owned(),
                            password.to_owned(),
                        ))
                    }
                    _ => (),
                },
            }
        }
        None
    }) {
        Ok(vnc) => vnc,
        Err(error) => {
            error!("cannot initialize VNC session: {}", error);
            std::process::exit(1)
        }
    };
    let mut fb_red_index = 0;
    #[cfg(feature = "eink_device")]
    let mut fb: Box<dyn Framebuffer> = if CURRENT_DEVICE.mark() != 8 {

        let raw_fb = KoboFramebuffer1::new(FB_DEVICE).context("can't create framebuffer").unwrap();
        fb_red_index = if raw_fb.var_info.red.offset > 0 && !red_shift { 2 } else { 0 };
        Box::new(raw_fb)

    } else {

        let raw_fb = KoboFramebuffer2::new(FB_DEVICE).context("can't create framebuffer").unwrap();
        fb_red_index = if raw_fb.var_info.red.offset > 0 && !red_shift { 2 } else { 0 };
        Box::new(raw_fb)

    };

    let (width, height) = vnc.size();
    info!(
        "connected to \"{}\", {}x{} framebuffer",
        vnc.name(),
        width,
        height
    );

    let SD_COLOR_FORMAT: PixelFormat = PixelFormat {
        bits_per_pixel: bits_format,
        depth: bits_format,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift:fb_red_index*8,
        green_shift: 8,
        blue_shift:(2-fb_red_index)*8,
    };
    // dbg!(red_shift,fb_red_index,bits_format, SD_COLOR_FORMAT.red_shift,SD_COLOR_FORMAT.blue_shift);
    //red shift and blue shift wrong thus color off... depth is not
    //og release only sampled red too... but it depends on bpp?
    //it depends on server it seems
    // const SD_COLOR_FORMAT: PixelFormat = PixelFormat {
    //     bits_per_pixel: 32,
    //     depth: 32,
    //     big_endian: false,
    //     true_colour: true,
    //     red_max: 255,
    //     green_max: 255,
    //     blue_max: 255,
    //     red_shift:0,
    //     green_shift: 8,
    //     blue_shift:16,
    // };


    let vnc_format = vnc.format();
    info!("received {:?}", vnc_format);

    vnc.set_format(SD_COLOR_FORMAT).unwrap();
    info!("enforced {:?}", SD_COLOR_FORMAT);

    vnc.set_encodings(&[Encoding::CopyRect, Encoding::Zrle])
        .unwrap();

    vnc.request_update(
        Rect {
            left: 0,
            top: 0,
            width,
            height,
        },
        false,
    )
        .unwrap();

    #[cfg(feature = "eink_device")]
    debug!(
        "running on device model=\"{}\" /dpi={} /dims={}x{}",
        CURRENT_DEVICE.model,
        CURRENT_DEVICE.dpi,
        CURRENT_DEVICE.dims.0,
        CURRENT_DEVICE.dims.1
    );



    #[cfg(feature = "eink_device")]
    {
        let startup_rotation = rotate;
        fb.set_rotation(startup_rotation).ok();
    }

    let post_proc_bin = PostProcBin {
        data: (0..=255)
            .map(|i| {
                if contrast_exp == 1.0 {
                    i
                } else {
                    let gray = contrast_gray_point;

                    let rem_gray = 255.0 - gray;
                    let inv_exponent = 1.0 / contrast_exp;

                    let raw_color = i as f32;
                    if raw_color < gray {
                        (gray * (raw_color / gray).powf(contrast_exp)) as u8
                    } else if raw_color > gray {
                        (gray + rem_gray * ((raw_color - gray) / rem_gray).powf(inv_exponent)) as u8
                    } else {
                        gray as u8
                    }
                }
            })
            .map(|i| -> u8 {
                if i > white_cutoff {
                    255
                } else {
                    i
                }
            })
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap(),
    };

    //const FRAME_MS: u64 = 1000 / 30;
    let FRAME_MS: u64 = (1000.0 / (fps as f64)) as u64;

    //const max_dirty_refreshes: usize = 500;
    let max_dirty_refreshes: usize = refresh as usize;

    let mut dirty_rects: Vec<Rectangle> = Vec::new();
    let mut dirty_rects_since_refresh: Vec<Rectangle> = Vec::new();
    let mut has_drawn_once = false;
    let mut dirty_update_count = 0;

    let mut time_at_last_draw = Instant::now();



    let mut paths = Vec::new();
    for ti in &TOUCH_INPUTS {
        if Path::new(ti).exists() {
            paths.push(ti.to_string());
            break;
        }
    }
    for bi in &BUTTON_INPUTS {
        if Path::new(bi).exists() {
            paths.push(bi.to_string());
            break;
        }
    }
    for pi in &POWER_INPUTS {
        if Path::new(pi).exists() {
            paths.push(pi.to_string());
            break;
        }
    }
    // println!("{:?}",paths);
    let (raw_sender, raw_receiver) = raw_events(paths);
    let touch_screen = gesture_events(device_events(raw_receiver, rotate));
    //let usb_port = usb_events();

    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();

    thread::spawn(move || {
        while let Ok(evt) = touch_screen.recv() {
            tx2.send(evt).ok();
        }
    });

    let mut fit_width:bool = false;
    let mut fit_height:bool = false;
    let mut scale_factor:f32 = 1.0;
    //dbg!(fb.width(),width,fb.height(),height);

    let mut x_padding = 0;
    let mut y_padding = 0;

    let mut x_offset:u32 = 0;
    let mut y_offset:u32 = 0;

    let mut left_x_truncate = 0;
    let mut top_y_truncate = 0;
    let mut right_x_truncate = 0;
    let mut bottom_y_truncate = 0;

    let mut device_fb_rect = rect![0, 0, fb.width() as i32, fb.height() as i32];
    let mut cropped_vnc_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, fb.width() as i32+x_padding as i32, fb.height() as i32+y_padding as i32];
    let mut original_vnc_fb_rect = rect![0, 0, width as i32, height as i32];
    let mut scaled_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, width as i32+x_padding as i32, height as i32+y_padding as i32];

    if scale {
        if width > height {
            //dbg!(fb.width(),width,fb.height(),height,(width as f32*scale_factor) as i32,(height as f32*scale_factor) as i32);
            fit_width = true;
            scale_factor = fb.width() as f32/width as f32;
            y_padding = ((fb.height()-(height as f32*scale_factor) as u32)/2) as u32;
            x_padding = 0;
            scaled_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, (width as f32*scale_factor) as i32, (height as f32*scale_factor) as i32];

        } else if height > width {
            //dbg!(fb.width(),width,fb.height(),height,(width as f32*scale_factor) as i32,(height as f32*scale_factor) as i32);
            fit_height = true;
            scale_factor = fb.height() as f32/height as f32;
            x_padding = ((fb.width() - (width as f32*scale_factor) as u32)/2) as u32;
            y_padding = 0;
            scaled_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, (width as f32*scale_factor) as i32, (height as f32*scale_factor) as i32];

        } else if height == width {
            if fb.height() > fb.width() {
                //dbg!(fb.width(),width,fb.height(),height,(width as f32*scale_factor) as i32,(height as f32*scale_factor) as i32);
                fit_width = true;
                //want to fit to smallest fb axis instead.
                scale_factor = fb.width() as f32/width as f32;
                x_padding = ((fb.width() - (width as f32*scale_factor) as u32)/2) as u32;
                y_padding = 0;
                scaled_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, (width as f32*scale_factor) as i32, (height as f32*scale_factor) as i32];

            } else {
                //dbg!(fb.width(),width,fb.height(),height,(width as f32*scale_factor) as i32,(height as f32*scale_factor) as i32);
                fit_height = true;
                scale_factor = fb.height() as f32/height as f32;
                y_padding = ((fb.height()-(height as f32*scale_factor) as u32)/2) as u32;
                x_padding = 0;
                scaled_fb_rect = rect![0+x_padding as i32, 0+y_padding as i32, (width as f32*scale_factor) as i32, (height as f32*scale_factor) as i32];

            }
        };
    } else {
        if width < fb.width() as u16 {
            x_padding = ((fb.width()-width as u32)/2) as u32
        };
        ;//width should always be smaller than or equal to fb width
        if height < fb.height() as u16 {
            y_padding = ((fb.height()-height as u32)/2) as u32;//if its bigger, it would fail anyway?
        };
        if width > fb.width() as u16 {
            cropped_vnc_fb_rect = rect![
                0+x_padding as i32+x_offset as i32,
                0+y_padding as i32+y_offset as i32,
                fb.width() as i32+x_padding as i32+x_offset as i32,
                fb.height() as i32+y_padding as i32+y_offset as i32];
        } else if height > fb.height() as u16 {
            cropped_vnc_fb_rect = rect![
                0+x_padding as i32+x_offset as i32,
                0+y_padding as i32+y_offset as i32,
                fb.width() as i32+x_padding as i32+x_offset as i32,
                fb.height() as i32+y_padding as i32+y_offset as i32];
        } else if width > fb.width() as u16 && height > fb.height() as u16 {
            cropped_vnc_fb_rect = rect![
                0+x_offset as i32,
                0+y_offset as i32,
                fb.width() as i32+x_offset as i32,
                fb.height() as i32+y_offset as i32];
        }

    };
    //dbg!(fb.width(),width,fb.height(),height,(width as f32*scale_factor) as i32,(height as f32*scale_factor) as i32);

    let full_update_mode = match full_update {
        1 => UpdateMode::Fast //a2
        ,
        2 => UpdateMode::FastMono //a2
        ,
        3 => UpdateMode::Gui //gc16 full
        ,
        4 => UpdateMode::Partial //gc16 hybrid
        ,
        5 => UpdateMode::Full
        ,
        _ => UpdateMode::Full //fast and fastmono are the same...
        ,
    };
    let partial_update_mode = match partial_update {
        1 => UpdateMode::Fast //a2
        ,
        2 => UpdateMode::FastMono //a2
        ,
        3 => UpdateMode::Gui //gc16 full
        ,
        4 => UpdateMode::Partial //gc16 hybrid
        ,
        5 => UpdateMode::Full
        ,
        _ => UpdateMode::Partial //fast and fastmono are the same...
        ,
    };
    match set_dither {
        true => fb.set_dithered(true),
        false => fb.set_dithered(false),
    };
    match set_monochrome {
        true => fb.set_monochrome(true),
        false => fb.set_monochrome(false),
    };

    let mut finger_down_count =  Instant::now();
    let finger_seconds = Duration::from_secs(2);

    'running: loop {
        //dbg!(left_x_truncate,right_x_truncate,top_y_truncate,bottom_y_truncate);
        if let Ok(evt) = rx.try_recv() {
            match evt {
                Event::Device(de) => {
                    match de {
                        DeviceEvent::Finger { id, time, status, position } => {
                            match id {
                                0 | 1 | 2 => {
                                    match status {
                                        FingerStatus::Up => { //we only want send right click once we release long_tap
                                            if scale {
                                                if long_tap {
                                                    if finger_down_count.elapsed() > finger_seconds {
                                                        vnc.send_pointer_event(0x04,
                                                                               (((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0, width as u16),
                                                                               (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0, height as u16)
                                                        ).unwrap();
                                                        vnc.send_pointer_event(0x00,
                                                                               (((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0, width as u16),
                                                                               (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0, height as u16)
                                                        ).unwrap();
                                                        //dbg!(((position.x as f32-x_padding as f32) / scale_factor) as u16, ((position.y as f32-y_padding as f32) / scale_factor) as u16);
                                                    }
                                                } else {
                                                    vnc.send_pointer_event(0x00,
                                                                           (((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0, width as u16),
                                                                           (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0, height as u16)
                                                    ).unwrap();
                                                    //dbg!(((position.x as f32 - x_padding as f32) / scale_factor) as u16, ((position.y as f32 - y_padding as f32) / scale_factor) as u16);
                                                }
                                            } else {
                                                if long_tap {
                                                    if finger_down_count.elapsed() > finger_seconds {
                                                        vnc.send_pointer_event(0x04,
                                                                               ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                               ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                        ).unwrap();
                                                        vnc.send_pointer_event(0x00,
                                                                               ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                               ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                        ).unwrap();
                                                        //dbg!(position.x as u16-x_padding as u16, position.y as u16-y_padding as u16);
                                                    }
                                                } else {
                                                    vnc.send_pointer_event(0x00,
                                                                           ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                           ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                    ).unwrap();
                                                    //dbg!(position.x as u16-x_padding as u16, position.y as u16-y_padding as u16);
                                                }
                                            };
                                            if finger_down_count.elapsed() > Duration::from_secs(6) {
                                                fb.set_rotation(CURRENT_DEVICE.startup_rotation()).ok();
                                                //drop(vnc);
                                                // Command::new("mnt/onboard/.adds/koreader/nickel.sh")
                                                //     //starting from ssh wont work...
                                                //     .status()
                                                //     .ok();
                                                break 'running;
                                            };
                                        },
                                        FingerStatus::Down => {
                                            if scale {
                                                vnc.send_pointer_event(0x01,
                                                                       (((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0, width as u16),
                                                                       (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0, height as u16)
                                                ).unwrap();
                                                finger_down_count = Instant::now();
                                                //dbg!((((position.x as f32 - x_padding as f32)/ scale_factor) as u16).clamp(0,width as u16), (((position.y as f32 - y_padding as f32)/ scale_factor) as u16).clamp(0,height as u16));
                                            } else {
                                                vnc.send_pointer_event(0x01,
                                                                       ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                       ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                ).unwrap();
                                                finger_down_count = Instant::now();
                                                //dbg!(position.x as u16-x_padding as u16,position.y as u16-y_padding as u16);
                                            }
                                        },
                                        FingerStatus::Motion => {
                                            if scale {
                                                vnc.send_pointer_event(0x01,
                                                                       (((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0, width as u16),
                                                                       (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0, height as u16)
                                                ).unwrap();
                                                //dbg!((((position.x as f32 - x_padding as f32) / scale_factor) as u16).clamp(0,width as u16), (((position.y as f32 - y_padding as f32) / scale_factor) as u16).clamp(0,height as u16));
                                                //100-10/2 45 100/2-10=40
                                                //from physical framebuffer means must minus padding before scale, scale is so original
                                            } else if is_swipe {

                                                vnc.send_pointer_event(0x00,
                                                                       ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                       ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                ).unwrap();

                                            } else {
                                                vnc.send_pointer_event(0x01,
                                                                       ((position.x as i16 - x_padding as i16+x_offset as i16) as u16).clamp(0, width as u16),
                                                                       ((position.y as i16 - y_padding as i16+y_offset as i16) as u16).clamp(0, height as u16)
                                                ).unwrap();
                                                //dbg!(position.x as u16-x_padding as u16, position.y as u16-y_padding as u16)
                                            }
                                        },
                                    }
                                },
                                _ => {
                                    println!("Unknown finger ID")
                                },
                            }
                        },
                        DeviceEvent::Button { code: ButtonCode::Power, status: ButtonStatus::Pressed, .. } => {
                            // println!("BUTTON");
                            fb.set_rotation(CURRENT_DEVICE.startup_rotation()).ok();
                            //drop(vnc);
                            Command::new("mnt/onboard/.adds/nickel.sh")
                                .status()
                                .ok();
                            break 'running;
                            //break;

                        },
                        DeviceEvent::CoverOn => {
                            // println!("COVER");
                            fb.set_rotation(CURRENT_DEVICE.startup_rotation()).ok();
                            //drop(vnc);
                            Command::new("mnt/onboard/.adds/nickel.sh")
                                //starting from ssh wont work...
                                .status()
                                .ok();
                            break 'running;
                            //break;
                        },
                        // DeviceEvent::Button { code: ButtonCode::Light, status: ButtonStatus::Pressed, .. } => {
                        //     tx.send(Event::ToggleFrontlight).ok();
                        // },
                        // DeviceEvent::RotateScreen(i8) => {
                        // },
                        _ => {},
                    }
                },
                Event::Gesture(ge) => {
                    match ge {
                        GestureEvent::Swipe {
                            dir, ..
                        } => {
                            dbg!(x_offset,y_offset,dir);
                            match dir {
                                Dir::North => {
                                    if height > fb.height() as u16 {
                                        has_drawn_once = false;
                                        if y_offset+fb.height()+fb.height()/2 < height as u32 {
                                            y_offset += fb.height() / 2;
                                        } else {
                                            y_offset += height as u32-fb.height()-y_offset;
                                        }
                                    }
                                },
                                Dir::East => {
                                    if width > fb.width() as u16 {
                                        has_drawn_once = false;
                                        //0-1920-379 >379
                                        if x_offset > fb.width() {
                                            x_offset -= fb.width() / 2;
                                        } else {
                                            x_offset = 0;
                                        }
                                    }
                                },
                                Dir::South => {
                                    if height > fb.height() as u16 {
                                        has_drawn_once = false;
                                        // 0 -1080/2 >1080/2
                                        if y_offset > fb.height() {
                                            y_offset -= fb.height() / 2;
                                        } else {
                                            y_offset = 0;
                                        }
                                    }
                                },
                                Dir::West => {
                                    if width > fb.width() as u16 {
                                        has_drawn_once = false;
                                        //0+758+758/2 < 1920
                                        if x_offset+fb.width()+fb.width()/2 < width as u32 {
                                            x_offset += fb.width() / 2;
                                        } else {
                                            // =1000+1920-758-1000
                                            x_offset += width as u32-fb.width()-x_offset;
                                        }
                                    }
                                },
                            }
                            if vnc.request_update(
                                Rect {
                                    left:0+x_offset as u16,
                                    top:0+y_offset as u16,
                                    width:fb.width() as u16,
                                    height:fb.height() as u16},
                                false,
                            ).is_err() {
                                error!("server disconnected");
                                break;
                            }
                            // fb.update(&device_fb_rect, full_update_mode).ok();
                            // dbg!(x_offset,y_offset);
                        },
                        _ => {
                        },
                    }
                },
            };
        };
        let time_at_sol = Instant::now();
        let mut frame_complete = false;
        let current_format = vnc.format();

        for event in vnc.poll_iter() {
            use client::Event;
            // dbg!(&event);
            match event {
                Event::Disconnected(None) => break 'running,
                Event::Disconnected(Some(error)) => {
                    error!("server disconnected: {:?}", error);
                    break 'running;
                }
                Event::PutPixels(vnc_rect, ref pixels) => {
                    debug!("Put pixels");

                    let elapsed_ms = time_at_sol.elapsed().as_millis();
                    debug!("network Δt: {}", elapsed_ms);

                    let bpp = current_format.bits_per_pixel as usize / 8;

                    if scale {
                        let scaled_l = (vnc_rect.left as f32 * scale_factor).round() as u32;
                        let scaled_t = (vnc_rect.top as f32 * scale_factor).round() as u32;
                        let scaled_r = ((vnc_rect.left + vnc_rect.width) as f32 * scale_factor).round() as u32;
                        let scaled_b = ((vnc_rect.top + vnc_rect.height) as f32 * scale_factor).round() as u32;

                        let scaled_rect_width = scaled_r - scaled_l;
                        let scaled_rect_height = scaled_b - scaled_t;

                        for y_out in 0..scaled_rect_height {
                            for x_out in 0..  scaled_rect_width {
                                let original_x = ((x_out as f32) / scale_factor);
                                let original_y = ((y_out as f32) / scale_factor);

                                let local_x = (original_x.round() as u32).clamp(0,(vnc_rect.width -1) as u32);
                                let local_y = (original_y.round() as u32).clamp(0,(vnc_rect.height -1) as u32);
                                // // sample pixel...
                                let src_idx = (local_y * vnc_rect.width as u32 + local_x) as usize;

                                if src_idx*bpp > pixels.len() {
                                    //dbg!(src_idx*bpp,pixels.len());
                                } else {
                                    let luma =
                                        if bpp >= 3 {
                                            let r = pixels[src_idx*bpp] as u32; //pixels is collection of bytes. u8. 4 bytes is 1 pixel.
                                            //oldest forced 8bits per pixel means 1 byte 1 is 1 pixel, if step by 4 then
                                            //only samplying every 4th pixel? if 8 bit forced would it still be vec of u8/ wouldnt it be vec of u2? 2 bits?
                                            //u8 used bc cpu is byte addressable, smallest unit
                                            let g = pixels[src_idx*bpp + 1] as u32;
                                            let b = pixels[src_idx*bpp + 2] as u32;
                                            (r * 299 + g * 587 + b * 114) / 1000
                                        } else if bpp == 2 {
                                            pixels[src_idx*bpp] as u32
                                            //rgb565?
                                        } else {
                                            pixels[src_idx] as u32
                                        };

                                    let gray = Color::Gray(post_proc_bin.data[luma as usize]);

                                    if blue_noise {
                                        fb.set_pixel(
                                            scaled_l + x_out+x_padding,
                                            scaled_t + y_out+y_padding,
                                            transform_dither_g2(scaled_l + x_out+x_padding, scaled_t + y_out+y_padding, gray));

                                    } else {
                                        fb.set_pixel(scaled_l + x_out+x_padding, scaled_t + y_out+y_padding, gray);
                                    };
                                }
                            }
                        }

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("postproc Δt: {}", elapsed_ms);
                            // } //5x3+4=19 x2=38 5x3x2+4x2=38 but 5x2x3x2+4x2=68
                            //draw gray_tile merely creates grayscale pixel vec, does not do drawing?
                            //actual pixel updating happens in client.rs fb.update method
                        //}
                        //there is no coord to say, draw rect at location. instead each pixel is drawn one by one...

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("draw Δt: {}", elapsed_ms);

                        let w = (vnc_rect.width as f32 * scale_factor).round();
                        let h = (vnc_rect.height as f32 * scale_factor).round();
                        let l = (vnc_rect.left as f32 * scale_factor).round();
                        let t = (vnc_rect.top as f32 * scale_factor).round();

                        let delta_rect = rect![
                            l as i32+x_padding as i32,
                            t as i32+y_padding as i32,
                            (l + w+x_padding as f32) as i32,
                            (t + h+y_padding as f32) as i32];
                        if delta_rect == scaled_fb_rect { //if rect sent is entire framebuffer, VNC framebuffer not Device framebuffer
                            dirty_rects.clear(); //clear tracking
                            dirty_rects_since_refresh.clear();
                            #[cfg(feature = "eink_device")]
                            {

                                if !has_drawn_once || dirty_update_count > max_dirty_refreshes { //if false which it is on first pass,
                                    // so if 500 frames, or rects? each event loop is 1 rect but multiple rects make up a frame.  have been processed
                                    // dirty update count is only updated at end of each frame...
                                    // , or if dirty count exceeded
                                    fb.update(&scaled_fb_rect, full_update_mode).ok();
                                    dirty_update_count = 0;
                                    has_drawn_once = true;
                                } else {
                                    fb.update(&scaled_fb_rect, partial_update_mode).ok();
                                    //otherwise if true or not yet reached max
                                }
                            }
                        } else {
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        } //if rect smaller than entire fb, add to dirty rect list

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("rects Δt: {}", elapsed_ms);

                    } else {

                        let w = vnc_rect.width as u32;
                        let h = vnc_rect.height as u32;
                        let l = vnc_rect.left as u32;
                        let t = vnc_rect.top as u32;

                        left_x_truncate = 0;
                        top_y_truncate = 0;
                        right_x_truncate = 0;
                        bottom_y_truncate = 0;

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("postproc Δt: {}", elapsed_ms);
                        //let bpp = current_format.bits_per_pixel as usize / 8;

                        //better check if in range than offset, range 0-758,758-1516,1616-1920
                        //0-500 250-750 500-1000, offset, width+offset
                        //we want to shift... by 50% of framebuffer each time?

                        if height > fb.height() as u16 {
                            if t > fb.height()+y_offset { continue };//if top is greater than upper limit
                            if t+h < y_offset { continue };//if bottom is less than lower limit
                        };

                        if width > fb.width() as u16 {
                            // if l > fb.width()+x_offset || l < x_offset { continue };
                            if l > fb.width()+x_offset { continue };//if left is greater than upper limit
                            if l+w < x_offset { continue };//if right is less than lower limit
                        };//left could be lower than limit and right could be more than upper, but doesnt mean whole rect is out of bounds

                        #[cfg(feature = "eink_device")]
                        {
                            'row: for row in 0..h {
                                'col: for col in 0..w {
                                    if height > fb.height() as u16 {
                                        if t + row < y_offset { //if y is less than lower limit, skip this pixel
                                            continue
                                        };
                                        if t + row == fb.height() + y_offset { //if y is greater than upper limit, break row loop?
                                            bottom_y_truncate = row;//break column loop? because the rect is done, no more pixels will be in bounds
                                            break 'row
                                        };
                                        //we have filtered out rects that are entirely out of bounds
                                        //now filter partial in bounds or, entirely in bounds
                                        if t + row == y_offset { //if y is greater than lower limit?
                                            top_y_truncate = row;//if exactly on limit, make truncate this y pixel
                                        };
                                    };
                                    if width > fb.width() as u16 {
                                        if l + col <  x_offset { //if x below lower limit skip this pixel
                                            continue
                                        }

                                        if l + col == fb.width() + x_offset { //if x is upper bound, i want to skip future x loops too
                                            right_x_truncate = col;//since the limit will be the same for each row... no, we only want to break this one
                                            break 'col //because we must still process the remaining pixels and set them
                                        }

                                        if l + col == x_offset { // a rect that is partial can only fulfill one
                                            //but a full bound rect can fulfill both conditions,
                                            // in which case truncate should be 0 but instead set to upper or lower limit, there is
                                            //only one truncate value, the line at which a rect is in bounds, is it possible a rect can be bigger
                                            //than current range and has 2 truncation lines? yes...
                                            left_x_truncate = col;//if x is lower bound
                                        }
                                    };
                                    //we only deal with coordinates, yea one co ordinate can never be smaller than min and bigger than ma

                                    //let c = Color::Gray(gray_pixels[(row * w + col) as usize]);
                                    //pixels is vec of u8, 1 byte per vector element
                                    //4 elements make one pixel
                                    let src_idx = (row*w+col) as usize;
                                    let luma =
                                        if bpp >= 3 {
                                            let r = pixels[src_idx*bpp] as u32; //pixels is collection of bytes. u8. 4 bytes is 1 pixel.
                                            //oldest forced 8bits per pixel means 1 byte 1 is 1 pixel, if step by 4 then
                                            //only samplying every 4th pixel? if 8 bit forced would it still be vec of u8/ wouldnt it be vec of u2? 2 bits?
                                            //u8 used bc cpu is byte addressable, smallest unit
                                            let g = pixels[src_idx*bpp+ 1] as u32;
                                            let b = pixels[src_idx*bpp + 2] as u32;
                                            (r * 299 + g * 587 + b * 114) / 1000
                                        } else if bpp == 2 {
                                            pixels[src_idx*bpp] as u32
                                            //rgb565?
                                        } else {
                                            pixels[src_idx] as u32
                                        };
                                    let gray = Color::Gray(post_proc_bin.data[luma as usize]);
                                    if blue_noise {
                                        fb.set_pixel(
                                            l + col-x_offset+x_padding, //although set pixel is for... device co ords, bc we are using vnc co ords must pan and subtract?
                                            t + row-y_offset+y_padding,
                                            transform_dither_g2(
                                                l + col-x_offset+x_padding,
                                                t + row-y_offset+y_padding,
                                                gray));

                                    } else {
                                        fb.set_pixel(
                                            l + col-x_offset + x_padding,
                                            t + row-y_offset + y_padding,
                                            gray);
                                        // dbg!(l + col-x_offset+x_padding, t + row-y_offset +y_padding, gray);
                                    };
                                }
                            }
                            //draw gray_tile merely creates grayscale pixel vec, does not do drawing?
                            //actual pixel updating happens in client.rs fb.update method
                        }
                        //there is no coord to say, draw rect at location. instead each pixel is drawn one by one into fb...
                        //and then update called separately

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("draw Δt: {}", elapsed_ms);

                        let mut w = vnc_rect.width as i32;
                        let mut h = vnc_rect.height as i32;
                        let l = vnc_rect.left as i32;
                        let t = vnc_rect.top as i32;

                        if right_x_truncate > 0 {
                            w = right_x_truncate as i32
                        }
                        if bottom_y_truncate > 0 {
                            h = bottom_y_truncate as i32
                        }

                        let delta_rect = rect![
                            l+x_padding as i32+left_x_truncate as i32-x_offset as i32,
                            t+y_padding as i32+top_y_truncate as i32-y_offset as i32,
                            l + w+x_padding as i32-x_offset as i32,
                            t + h+y_padding as i32-y_offset as i32];
                        cropped_vnc_fb_rect = rect![
                            0+x_padding as i32+x_offset as i32,
                            0+y_padding as i32+y_offset as i32,
                            fb.width() as i32+x_padding as i32+x_offset as i32,
                            fb.height() as i32+y_padding as i32+y_offset as i32];
                        //cropped_vnc gives location in vnc space, while delta rect must use dev fb space otherwise fb.update will fail
                        //-xoffset vs +x_offset will also ensure they never equal to each othter...
                        //delta rect and dev fb rect... can be equal to each other in size but in different location?
                        //since we always subtract offset... we could receive a rect entirely out of bounds same size... but because we discard rects out of bounds
                        //were fine?

                        if delta_rect == original_vnc_fb_rect || delta_rect == cropped_vnc_fb_rect || delta_rect == device_fb_rect {
                            dirty_rects.clear();
                            dirty_rects_since_refresh.clear();
                            #[cfg(feature = "eink_device")]
                            {
                                if !has_drawn_once || dirty_update_count > max_dirty_refreshes {
                                    fb.update(&device_fb_rect, full_update_mode).ok();
                                    dirty_update_count = 0;
                                    has_drawn_once = true;
                                } else {
                                    fb.update(&device_fb_rect, partial_update_mode).ok();
                                }
                            }
                        } else {
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        }

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("rects Δt: {}", elapsed_ms);
                    };
                    // Single pass: convert to grayscale + apply post-processing LUT.
                    // Use the current negotiated format (may have changed via set_format).

                }

                Event::CopyPixels { src, dst } => {
                    debug!("Copy pixels!");

                    #[cfg(feature = "eink_device")]
                    {
                        if scale {
                            {
                                if (src.width as f32 * scale_factor).round() as u32 == 0 ||
                                    (src.height as f32 * scale_factor).round() as u32 == 0 {
                                    continue
                                }

                                let src_left = (src.left as f32 * scale_factor);
                                let src_top = (src.top as f32 * scale_factor);

                                let dst_left = (dst.left as f32 * scale_factor);
                                let dst_top = (dst.top as f32 * scale_factor);

                                let mut intermediary_pixmap =
                                    Pixmap::new((dst.width as f32*scale_factor).round() as u32,
                                                (dst.height as f32*scale_factor).round() as u32,
                                                CURRENT_DEVICE.color_samples());

                                for y in 0..intermediary_pixmap.height {//copypixels merely copy whats on framebuffer, if putpixels blue noise dithered so will copied
                                    for x in 0..intermediary_pixmap.width {
                                        let color = fb.get_pixel((src_left + x as f32+x_padding as f32).round() as u32, (src_top + y as f32+y_padding as f32).round() as u32);
                                        intermediary_pixmap.set_pixel(x, y, color);
                                    }
                                }

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = intermediary_pixmap.get_pixel(x, y);
                                        fb.set_pixel((dst_left + x as f32+x_padding as f32).round() as u32, (dst_top + y as f32+y_padding as f32).round() as u32, color);
                                    }
                                }
                            }

                            let delta_rect = rect![
                        (dst.left as f32 * scale_factor).round() as i32+x_padding as i32,
                        (dst.top as f32 * scale_factor).round() as i32+y_padding as i32,
                        ((dst.left as f32 * scale_factor) + dst.width as f32).round() as i32+x_padding as i32,
                        ((dst.top as f32 * scale_factor) + dst.height as f32).round() as i32+y_padding as i32
                        ];
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect); //add to dirty rect list, merely copy rect to another place, no update call

                        } else {
                            let src_left = src.left as u32;
                            let src_top = src.top as u32;
                            let src_width = src.width as u32;
                            let src_height = src.height as u32;

                            let dst_left = dst.left as u32;
                            let dst_top = dst.top as u32;
                            let mut dst_width = dst.width as u32;
                            let mut dst_height = dst.height as u32;

                            left_x_truncate = 0;
                            top_y_truncate = 0;
                            right_x_truncate = 0;
                            bottom_y_truncate = 0;

                            {

                                if height > fb.height() as u16 {
                                    if dst_top > fb.height()+y_offset  { continue };//if top is greater than upper
                                    if dst_top+dst_height < y_offset { continue };//if bot is less than lower
                                }

                                if width > fb.width() as u16 {
                                    if dst_left > fb.width()+x_offset { continue };//if left is greater than upper
                                    if dst_left+dst_width < x_offset { continue };//if right is less than lower
                                }

                                let mut intermediary_pixmap =
                                    Pixmap::new(dst.width as u32,
                                                dst.height as u32,
                                                CURRENT_DEVICE.color_samples());

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = fb.get_pixel(src_left + x+x_padding-x_offset as u32, src_top + y+y_padding-y_offset as u32);
                                        intermediary_pixmap.set_pixel(x, y, color);
                                    }
                                }

                                'y:for y in 0..intermediary_pixmap.height {
                                    'x:for x in 0..intermediary_pixmap.width {
                                        let color = intermediary_pixmap.get_pixel(x, y);
                                        if height > fb.height() as u16 {
                                            if y + dst_top == fb.height() + y_offset {
                                                bottom_y_truncate = y;
                                                break 'y
                                            } //if y pixel is greater than upper

                                            if y + dst_top <  y_offset {
                                                continue
                                            }//do we want continue or break first? which saves cycles?

                                            if y + dst_top == y_offset {
                                                top_y_truncate = y;
                                            } //if y less than lower, once hits lower limit
                                        };
                                        if width > fb.width() as u16 {
                                            if x + dst_left == fb.width() + x_offset {
                                                right_x_truncate = x;
                                                break 'x
                                            }
                                            if x + dst_left < x_offset {
                                                continue
                                            }
                                            if x + dst_left == x_offset {
                                                left_x_truncate = x;
                                            }
                                        };
                                        // fb.set_pixel(dst_left + x, dst_top + y,  transform_dither_g2(dst_left + x, dst_top + y,color));
                                        fb.set_pixel(dst_left + x-x_offset+x_padding as u32, dst_top + y-y_offset+y_padding as u32, color);
                                    }
                                }
                            }
                            if right_x_truncate > 0 {
                                dst_width = right_x_truncate
                            }
                            if bottom_y_truncate > 0 {
                                dst_height = bottom_y_truncate
                            }

                            let delta_rect = rect![
                        dst_left as i32+x_padding as i32+left_x_truncate as i32-x_offset as i32,
                        dst_top as i32+y_padding as i32+top_y_truncate as i32-y_offset as i32,
                        (dst_left + dst_width) as i32+x_padding as i32-x_offset as i32,
                        (dst_top + dst_height) as i32+y_padding as i32-y_offset as i32
                    ];
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        };
                    }
                }
                Event::EndOfFrame => {
                    debug!("End of frame!");

                    if !has_drawn_once { //if false, which on 1st loop is true, but by end of frame should be true now. so only do this if false
                        has_drawn_once = dirty_rects.len() > 0; //set to if dirty rects is not empty, which it is on 1st loop but not on subsequent
                        //but if drawn is already true, then dont worry about it
                    }

                    dirty_update_count += 1;

                    if dirty_update_count > max_dirty_refreshes {
                        info!("Full refresh!");
                        for dr in &dirty_rects_since_refresh {
                            #[cfg(feature = "eink_device")]
                            {
                                fb.update(&dr, full_update_mode).ok();
                            }
                        }//earlier only triggered if dirty max hit and rect was entire fb size
                        dirty_update_count = 0;
                        dirty_rects_since_refresh.clear();//clear since list but not dirty rect list?
                    } else { //if not yet reached 500 frames
                        for dr in &dirty_rects {
                            debug!("Updating dirty rect {:?}", dr);

                            #[cfg(feature = "eink_device")]
                            {
                                if dr.height() < 100 && dr.width() < 100 {
                                    debug!("Fast mono update!"); //if rect is smaller than
                                    fb.update(&dr, partial_update_mode/*UpdateMode::FastMono DU? or A2? Partial GC16 used never a2...*/ ).ok();
                                } else {
                                    fb.update(&dr, partial_update_mode/*UpdateMode::Partial GC16*/).ok();
                                }
                            }

                            push_to_dirty_rect_list(&mut dirty_rects_since_refresh, *dr);
                            //add updates rects to tracking so will know how
                            //this list is only updated after the dirty rect has been updated?
                            //in which case it will be updated again if we hit max dirty frames of 500?
                            //it will only be cleared if max dirty refresh AND full framebuffer rect encountered,
                            //or after 500 frames
                            // if rect is entire fb, 1st time draw anything or max frames reached: if !has_drawn_once || dirty_update_count > max_dirty_refreshes
                            //once set true never again returns to false, thus on first draw
                        }

                        time_at_last_draw = Instant::now();
                    }

                    dirty_rects.clear();
                    //regardless of anything clear it at end of each frame? but keep dirty rect since list?
                    // unless we have hit max refreshes or first draw and entire frame is the rect

                    frame_complete = true;
                }
                // x => info!("{:?}", x), /* ignore unsupported events */
                _ => (),
            }
        }

        if frame_complete {
            if scale {
                if vnc.request_update(
                    Rect { left: 0, top: 0, width, height },
                    true,
                ).is_err() {
                    error!("server disconnected");
                    break;
                }
            } else {
                if vnc.request_update(
                    Rect {
                        left:0+x_offset as u16,
                        top:0+y_offset as u16,
                        width:if width < fb.width() as u16 {width} else {fb.width() as u16},
                        height:if height < fb.height() as u16 {height} else {fb.height() as u16}
                    },
                    true,
                ).is_err() {
                    error!("server disconnected");
                    break;
                }
            }
        }
        //only at end of frame request a new update

        if FRAME_MS > time_at_sol.elapsed().as_millis() as u64 {
            if dirty_rects_since_refresh.len() > 0 && time_at_last_draw.elapsed().as_secs() > 3 {
                for dr in &dirty_rects_since_refresh {
                    #[cfg(feature = "eink_device")]
                    {
                        fb.update(&dr, full_update_mode).ok();
                    }
                }
                dirty_update_count = 0;
                dirty_rects_since_refresh.clear();
            }

            if FRAME_MS > time_at_sol.elapsed().as_millis() as u64 {
                thread::sleep(Duration::from_millis(
                    FRAME_MS - time_at_sol.elapsed().as_millis() as u64,
                ));
            }
        } else {
            info!(
                "Missed frame, excess Δt: {}ms",
                time_at_sol.elapsed().as_millis() as u64 - FRAME_MS
            );
        }
    }

    Ok(())
}

fn push_to_dirty_rect_list(list: &mut Vec<Rectangle>, rect: Rectangle) {
    for dr in list.iter_mut() {
        if dr.contains(&rect) {
            return;
        }
        if rect.contains(&dr) {
            *dr = rect;
            return;
        }
        if rect.extends(&dr) {
            dr.absorb(&rect);
            return;
        }
    }

    list.push(rect);
}