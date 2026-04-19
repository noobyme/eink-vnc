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

use crate::framebuffer::{Framebuffer, KoboFramebuffer1, KoboFramebuffer2, Pixmap, UpdateMode};
use crate::geom::Rectangle;
use crate::vnc::{client, Client, Encoding, Rect};
use clap::{value_t, App, Arg};
use log::{debug, error, info};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use vnc::PixelFormat;
use input::{raw_events, device_events, usb_events, display_rotate_event, button_scheme_event, DeviceEvent, FingerStatus};

use anyhow::{Context as ResultExt, Error};

use crate::device::CURRENT_DEVICE;
use std::fs::File;
use std::io::Read;
use std::mem;
use std::slice;
//use std::thread;
use std::path::Path;
use std::sync::mpsc;

const FB_DEVICE: &str = "/dev/fb0";

const TOUCH_INPUTS: [&str; 5] = ["/dev/input/by-path/platform-2-0010-event",
    "/dev/input/by-path/platform-1-0038-event",
    "/dev/input/by-path/platform-1-0010-event",
    "/dev/input/by-path/platform-0-0010-event",
    "/dev/input/event1"];
const SD_COLOR_FORMAT: PixelFormat = PixelFormat {
    bits_per_pixel: 8,
    depth: 16,
    big_endian: false,
    true_colour: true,
    red_max: 255,
    green_max: 255,
    blue_max: 255,
    red_shift: 16,
    green_shift: 8,
    blue_shift: 0,
};

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
                .index(1),
        )
        .arg(
            Arg::with_name("PORT")
                .help("server port (default: 5900)")
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
        ).arg(
        Arg::with_name("ROTATE")
            .help("rotation (1-4), tested on a Clara HD, try at own risk")
            .long("rotate")
            .takes_value(true),
        ).arg(
            Arg::with_name("SCALE")
                .help("fit to height or width")
                .long("scale")
        ).arg(
            Arg::with_name("LONGTAP")
                .help("long tap to send right click, for pc servers. not necessary for touchscreen servers")
                .long("longtap")
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
    let longtap = matches.is_present("LONGTAP");

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

    let (width, height) = vnc.size();
    info!(
        "connected to \"{}\", {}x{} framebuffer",
        vnc.name(),
        width,
        height
    );

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
    let mut fb: Box<dyn Framebuffer> = if CURRENT_DEVICE.mark() != 8 {
        Box::new(
            KoboFramebuffer1::new(FB_DEVICE)
                .context("can't create framebuffer")
                .unwrap(),
        )
    } else {
        Box::new(
            KoboFramebuffer2::new(FB_DEVICE)
                .context("can't create framebuffer")
                .unwrap(),
        )
    };

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

    const FRAME_MS: u64 = 1000 / 30;

    const MAX_DIRTY_REFRESHES: usize = 500;

    let mut dirty_rects: Vec<Rectangle> = Vec::new();
    let mut dirty_rects_since_refresh: Vec<Rectangle> = Vec::new();
    let mut has_drawn_once = false;
    let mut dirty_update_count = 0;

    let mut time_at_last_draw = Instant::now();

    let fb_rect = rect![0, 0, width as i32, height as i32];

    let mut paths = Vec::new();
    for ti in &TOUCH_INPUTS {
        if Path::new(ti).exists() {
            paths.push(ti.to_string());
            break;
        }
    }
    // for bi in &BUTTON_INPUTS {
    //     if Path::new(bi).exists() {
    //         paths.push(bi.to_string());
    //         break;
    //     }
    // }
    // for pi in &POWER_INPUTS {
    //     if Path::new(pi).exists() {
    //         paths.push(pi.to_string());
    //         break;
    //     }
    // }

    let (raw_sender, raw_receiver) = raw_events(paths);
    let touch_screen = device_events(raw_receiver, rotate);
    //let usb_port = usb_events();

    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();

    thread::spawn(move || {
        while let Ok(evt) = touch_screen.recv() {
            tx2.send(evt).ok();
        }
    });

    // let mut dims_x = CURRENT_DEVICE.dims.1;
    // let mut dims_y = CURRENT_DEVICE.dims.0;
    // if CURRENT_DEVICE.should_swap_axes(rotation) {
    //     //mem::swap(&mut tc.x, &mut tc.y);
    //     dims_x = CURRENT_DEVICE.dims.0;
    //     dims_y = CURRENT_DEVICE.dims.1;
    // }


    let mut fit_width:bool = false;
    let mut fit_height:bool = false;
    let mut scale_factor:f32 = 1.0;
    let scaled_resolution:(u32,u32);
    let total_scaled_pixels:u32;
    //let src_index_vec:Vec<u32>;
    let mut src_index_vec: Vec<u32> = Vec::new();

    fn gen_scale_index(input:u32, factor:f32, vnc_width:u16, vnc_height:u16, scaled_res_x:u32,scaled_res_y:u32) -> Vec<u32> {
        (0..input)
            .map(|location| {
                //get location of scaled index from scaled resolution vector, essentially fb width or height, whichever is fit
                let x_in = location % scaled_res_x; //get remainder
                let y_in = location / scaled_res_x; //get quotient?

                //get location of original index from original resolution vector, vnc width height
                // let x_out = (x_in as f32 / factor).floor() as u32; //if OG 500, FIT/FB 1000 =0.5 500*0.5=250 500/0.5=1000
                // let y_out = (y_in as f32 / factor).floor() as u32; //do opposite of what scale factor does, thus multiply

                let x_out = ((x_in as f32 / factor).floor() as isize)
                    .clamp(0, vnc_width as isize - 1) as u32;

                let y_out = ((y_in as f32 / factor).floor() as isize)
                    .clamp(0, vnc_height as isize - 1) as u32;

                let src_index = y_out * vnc_width as u32 + x_out; //vnc width... or fb width? vnc

                src_index
            }
        )
        .collect()
    }
    //println!("fbw{:?}fbh{:?}vncw{:?}vnch{:?}",fb.width(),fb.height(),width,height);
    if scale {
        if width > height {
            fit_width = true;
            scale_factor = fb.width() as f32/width as f32;
            //scaled_resolution = (fb.width(), (height as f32 * scale_factor) as u32);
            //total_pixels = fb.width() * width as u32;
            //total_scaled_pixels = scaled_resolution.0 * scaled_resolution.1;
            //src_index_vec = gen_scale_index(total_scaled_pixels,scale_factor,width,height,scaled_resolution.0,scaled_resolution.1);

        } else if height > width {
            fit_height = true;
            scale_factor = fb.height() as f32/height as f32;
            // scaled_resolution = ((width as f32 * scale_factor) as u32, fb.height() as u32);
            // //total_pixels = fb.width() * width as u32;
            // total_scaled_pixels = scaled_resolution.0 * scaled_resolution.1;
            // src_index_vec = gen_scale_index(total_scaled_pixels,scale_factor,width,height,scaled_resolution.0,scaled_resolution.1);

        } else if height == width {
            if fb.height() > fb.width() {
                fit_width = true;
                //want to fit to smallest fb axis instead.
                scale_factor = fb.width() as f32/width as f32;
                // scaled_resolution = (fb.width(), (height as f32 * scale_factor) as u32);
                // total_pixels = fb.width() * width as u32;
                // total_scaled_pixels = scaled_resolution.0 * scaled_resolution.1;
                // src_index_vec = gen_scale_index(total_scaled_pixels,scale_factor,width,height,scaled_resolution.0,scaled_resolution.1);

            } else {
                fit_height = true;
                //want to fit to smallest fb axis instead.
                scale_factor = fb.height() as f32/height as f32;
            //     scaled_resolution = ((width as f32 * scale_factor) as u32, fb.height() as u32);
            //     //total_pixels = fb.width() * width as u32;
            //     total_scaled_pixels = scaled_resolution.0 * scaled_resolution.1;
            //     src_index_vec = gen_scale_index(total_scaled_pixels,scale_factor,width,height,scaled_resolution.0,scaled_resolution.1);
            }
        };
    } else {
    };

    let mut finger_down_count =  Instant::now();
    let finger_seconds = Duration::from_secs(2);
    'running: loop {
        // plato_to_vnc_touch(scale_factor,scale,longtap,finger_down_count,finger_seconds);
        if let Ok(evt) = rx.try_recv() {
            match evt {
                DeviceEvent::Finger { id, time, status, position }  => {
                    match id {
                        0 => {
                            match status {
                                FingerStatus::Up => {//we only want send right click once we release longtap
                                    if scale {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    } else {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, position.x as u16, position.y as u16).unwrap();
                                                vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    };

                                },
                                FingerStatus::Down => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    }

                                },
                                FingerStatus::Motion => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                    }
                                },
                            }
                        },
                        1 => {
                            match status {
                                FingerStatus::Up => {//we only want send right click once we release longtap
                                    if scale {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    } else {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, position.x as u16, position.y as u16).unwrap();
                                                vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    };

                                },
                                FingerStatus::Down => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    }

                                },
                                FingerStatus::Motion => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                    }
                                },
                            }
                        },
                        2 => {
                            match status {
                                FingerStatus::Up => {//we only want send right click once we release longtap
                                    if scale {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    } else {
                                        if longtap {
                                            if finger_down_count.elapsed() > finger_seconds {
                                                vnc.send_pointer_event(0x04, position.x as u16, position.y as u16).unwrap();
                                                vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                                //println!("LongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            }
                                        }
                                        else {
                                            vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                            // println!("NoLongTap{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                                            // println!("Up{:?},{:?}",finger_down_count,longtap);
                                        }
                                    };

                                },
                                FingerStatus::Down => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                        finger_down_count = Instant::now();
                                    }

                                },
                                FingerStatus::Motion => {
                                    if scale {
                                        vnc.send_pointer_event(0x01, (position.x as f32 / scale_factor).floor() as u16,  (position.y as f32 / scale_factor).floor() as u16).unwrap();
                                    } else {
                                        vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                    }
                                },
                            }
                        },
                        _ => {
                            eprintln!("Unknown finger ID")
                        },
                    }
                },
                // DeviceEvent::Button => {
                // },
                // DeviceEvent::RotateScreen(i8) => {
                // },
                _ => {}
            };
        };
        let time_at_sol = Instant::now();
        let mut frame_complete = false;
        let current_format = vnc.format();

        for event in vnc.poll_iter() {
            use client::Event;

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

                    //println!("VNCRW{:?}VNCRH{:?}TOTALPIX{:?}BPP{:?}",vnc_rect.width,vnc_rect.height,pixels.len(),bpp);

                    //turns into bytes i see, bits to bytes
                    // let scale_down =
                    //     pixels
                    //         .chunks_exact(bpp)
                    //         .map(|p|)
                    //         .collect();
                    if scale {
                        let mut vnc_rect_index:Vec<u32>= Vec::new();
                        if ((vnc_rect.width as f32)*scale_factor).floor() as u32 == 0
                            || ((vnc_rect.height as f32)*scale_factor).floor() as u32 == 0 {
                            continue;
                            //println!("SKIP,TOTAL{:?}SF{:?}VNC_RW{:?}VNC_RH{:?}VNCRSW{:?}VNCRSH{:?}",(pixels.len()/bpp) as u32,
                            //          scale_factor, vnc_rect.width, vnc_rect.height,
                            //          ((vnc_rect.width as f32)*scale_factor) as u32,
                            //          ((vnc_rect.height as f32)*scale_factor) as u32);
                        } else {
                            vnc_rect_index = gen_scale_index((((vnc_rect.width as f32)*scale_factor).floor()*((vnc_rect.height as f32)*scale_factor).floor()) as u32
                                                             /*(pixels.len()/bpp) as u32*/,
                                                                 scale_factor, vnc_rect.width, vnc_rect.height,
                                                                 ((vnc_rect.width as f32)*scale_factor).floor() as u32,
                                                                 ((vnc_rect.height as f32)*scale_factor).floor() as u32);
                        };

                        // println!("TOTAL{:?}SF{:?}VNC_RW{:?}VNC_RH{:?}VNCRSW{:?}VNCRSH{:?}",(pixels.len()/bpp) as u32,
                        //          scale_factor, vnc_rect.width, vnc_rect.height,
                        //          ((vnc_rect.width as f32)*scale_factor).floor() as u32,
                        //          ((vnc_rect.height as f32)*scale_factor).floor() as u32);

                        let gray_pixels: Vec<u8> = vnc_rect_index
                            .iter()
                            .map(|i| {
                                let base = (*i as usize) * bpp;
                                //original only sampled blue, is it faster?
                                let luma = if bpp >= 3 {
                                    (pixels[base] as u32 * 299 //B?
                                        + pixels[base + 1] as u32 * 587 //G?
                                        + pixels[base + 2] as u32 * 114) / 1000 //R?
                                } else {
                                    pixels[base] as u32
                                    //bpp is bytes not bits
                                    //if only 2 or 4 level color... then luma = base?
                                    //means each RGB value has 8 bits, 256 levels of red green or blue for each pixel?
                                    //if only 2 bits, then red level is either 1,2,3 or 4?.
                                    //no, entire pixel itself only has 4 colors, 1,2,3 or 4.
                                    //so +1 means add one byte, because pixels is a &[u8]
                                    //1bpp is 8 bit, 2 is 16bit, 3 is 24 bit, 4 is 32bit.
                                };

                                post_proc_bin.data[luma as usize]//0-255 values, check
                                //postproc bin what value returns? applies filter defined by cli flags
                            })
                            .collect();

                        //      let gray_pixels: Vec<u8> = if bpp >= 3 {
                        //     // 32bpp or 24bpp: compute luminance from RGB channels.
                        //     // Server bytes: [R, G, B, ...] (red_shift=0, green_shift=8, blue_shift=16)
                        //     pixels
                        //         .chunks_exact(bpp)
                        //         .map(|p| {
                        //             let luma = (p[0] as u32 * 299
                        //                 + p[1] as u32 * 587
                        //                 + p[2] as u32 * 114)
                        //                 / 1000;
                        //             post_proc_bin.data[luma as usize]
                        //         })
                        //         .collect()
                        // } else {
                        //     // 8bpp: each byte is already a single-channel value, apply LUT directly.
                        //     pixels
                        //         .iter()
                        //         .map(|&c| post_proc_bin.data[c as usize])
                        //         .collect()
                        // };

                        let w = (vnc_rect.width as f32 * scale_factor).floor() as u32;
                        let h = (vnc_rect.height as f32 * scale_factor).floor() as u32;
                        let l = (vnc_rect.left as f32 * scale_factor).floor() as u32;
                        let t = (vnc_rect.top as f32 * scale_factor).floor() as u32;

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("postproc Δt: {}", elapsed_ms);
                        //println!("W{:?}H{:?}L{:?}T{:?}GPLEN{:?}",w,h,l,t,gray_pixels.len());
                        // dbg!(vnc_rect_index.len());
                        #[cfg(feature = "eink_device")]
                        {
                            fb.draw_gray_tile(l, t, w, h, &gray_pixels);
                            //draw gray_tile merely creates grayscale pixel vec, does not do drawing?
                            //actual pixel updating happens in client.rs fb.update method
                        }
                        //there is no coord to say, draw rect at location. instead each pixel is drawn one by one...

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("draw Δt: {}", elapsed_ms);

                        let w = (vnc_rect.width as f32 * scale_factor).floor() as i32;
                        let h = (vnc_rect.height as f32 * scale_factor).floor() as i32;
                        let l = (vnc_rect.left as f32 * scale_factor).floor() as i32;
                        let t = (vnc_rect.top as f32 * scale_factor).floor() as i32;


                        let delta_rect = rect![l, t, l + w, t + h];
                        if delta_rect == fb_rect {
                            dirty_rects.clear();
                            dirty_rects_since_refresh.clear();
                            #[cfg(feature = "eink_device")]
                            {
                                if !has_drawn_once || dirty_update_count > MAX_DIRTY_REFRESHES {
                                    fb.update(&fb_rect, UpdateMode::Full).ok();
                                    dirty_update_count = 0;
                                    has_drawn_once = true;
                                } else {
                                    fb.update(&fb_rect, UpdateMode::Partial).ok();
                                }
                            }
                        } else {
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        }

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("rects Δt: {}", elapsed_ms);
                    } else {
                        let gray_pixels: Vec<u8> = if bpp >= 3 {
                            // 32bpp or 24bpp: compute luminance from RGB channels.
                            // Server bytes: [R, G, B, ...] (red_shift=0, green_shift=8, blue_shift=16)
                            pixels
                                .chunks_exact(bpp)
                                .map(|p| {
                                    let luma = (p[0] as u32 * 299
                                        + p[1] as u32 * 587
                                        + p[2] as u32 * 114)
                                        / 1000;
                                    post_proc_bin.data[luma as usize]
                                })
                                .collect()
                        } else {
                            // 8bpp: each byte is already a single-channel value, apply LUT directly.
                            pixels
                                .iter()
                                .map(|&c| post_proc_bin.data[c as usize])
                                .collect()
                        };

                        let w = vnc_rect.width as u32;
                        let h = vnc_rect.height as u32;
                        let l = vnc_rect.left as u32;
                        let t = vnc_rect.top as u32;

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("postproc Δt: {}", elapsed_ms);

                        #[cfg(feature = "eink_device")]
                        {
                            fb.draw_gray_tile(l, t, w, h, &gray_pixels);
                            //draw gray_tile merely creates grayscale pixel vec, does not do drawing?
                            //actual pixel updating happens in client.rs fb.update method
                        }
                        //there is no coord to say, draw rect at location. instead each pixel is drawn one by one into fb...
                        //and then update called separately

                        let elapsed_ms = time_at_sol.elapsed().as_millis();
                        debug!("draw Δt: {}", elapsed_ms);

                        let w = vnc_rect.width as i32;
                        let h = vnc_rect.height as i32;
                        let l = vnc_rect.left as i32;
                        let t = vnc_rect.top as i32;

                        let delta_rect = rect![l, t, l + w, t + h];
                        if delta_rect == fb_rect {
                            dirty_rects.clear();
                            dirty_rects_since_refresh.clear();
                            #[cfg(feature = "eink_device")]
                            {
                                if !has_drawn_once || dirty_update_count > MAX_DIRTY_REFRESHES {
                                    fb.update(&fb_rect, UpdateMode::Full).ok();
                                    dirty_update_count = 0;
                                    has_drawn_once = true;
                                } else {
                                    fb.update(&fb_rect, UpdateMode::Partial).ok();
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
                        // if scale {
                        //     let vnc_rect_index:Vec<u32>= Vec::new();
                        //     if ((vnc_rect.width as f32)*scale_factor).floor() as u32 == 0
                        //         || ((vnc_rect.height as f32)*scale_factor).floor() as u32 == 0 {
                        //         continue;
                        //         println!("SKIP,TOTAL{:?}SF{:?}VNC_RW{:?}VNC_RH{:?}VNCRSW{:?}VNCRSH{:?}",(pixels.len()/bpp) as u32,
                        //                  scale_factor, vnc_rect.width, vnc_rect.height,
                        //                  ((vnc_rect.width as f32)*scale_factor) as u32,
                        //                  ((vnc_rect.height as f32)*scale_factor) as u32);
                        //     } else {
                        //         let vnc_rect_index = gen_scale_index((pixels.len()/bpp) as u32,
                        //                                              scale_factor, vnc_rect.width, vnc_rect.height,
                        //                                              ((vnc_rect.width as f32)*scale_factor) as u32,
                        //                                              ((vnc_rect.height as f32)*scale_factor) as u32);
                        //     };
                        if scale {
                            {
                                if (src.width as f32 * scale_factor).floor() as u32 == 0 ||
                                    (src.height as f32 * scale_factor).floor() as u32 == 0 {
                                    continue
                                }

                                let src_left = (src.left as f32 * scale_factor).floor() as u32;
                                let src_top = (src.top as f32 * scale_factor).floor() as u32;

                                let dst_left = (dst.left as f32 * scale_factor).floor() as u32;
                                let dst_top = (dst.top as f32 * scale_factor).floor() as u32;

                                let mut intermediary_pixmap =
                                    Pixmap::new((dst.width as f32*scale_factor).floor() as u32, (dst.height as f32*scale_factor).floor() as u32);

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = fb.get_pixel(src_left + x, src_top + y);
                                        intermediary_pixmap.set_pixel(x, y, color);
                                    }
                                }

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = intermediary_pixmap.get_pixel(x, y);
                                        fb.set_pixel(dst_left + x, dst_top + y, color);
                                    }
                                }
                            }

                            let delta_rect = rect![
                        (dst.left as f32 * scale_factor).floor() as i32,
                        (dst.top as f32 * scale_factor).floor() as i32,
                        ((dst.left as f32 * scale_factor).floor() + dst.width as f32) as i32,
                        ((dst.top as f32 * scale_factor).floor() + dst.height as f32) as i32
                        ];
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);

                        } else {
                            {
                                let src_left = src.left as u32;
                                let src_top = src.top as u32;

                                let dst_left = dst.left as u32;
                                let dst_top = dst.top as u32;

                                let mut intermediary_pixmap =
                                    Pixmap::new(dst.width as u32, dst.height as u32);

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = fb.get_pixel(src_left + x, src_top + y);
                                        intermediary_pixmap.set_pixel(x, y, color);
                                    }
                                }

                                for y in 0..intermediary_pixmap.height {
                                    for x in 0..intermediary_pixmap.width {
                                        let color = intermediary_pixmap.get_pixel(x, y);
                                        fb.set_pixel(dst_left + x, dst_top + y, color);
                                    }
                                }
                            }

                            let delta_rect = rect![
                        dst.left as i32,
                        dst.top as i32,
                        (dst.left + dst.width) as i32,
                        (dst.top + dst.height) as i32
                    ];
                            push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        };
                    }
                }
                Event::EndOfFrame => {
                    debug!("End of frame!");

                    if !has_drawn_once {
                        has_drawn_once = dirty_rects.len() > 0;
                    }

                    dirty_update_count += 1;

                    if dirty_update_count > MAX_DIRTY_REFRESHES {
                        info!("Full refresh!");
                        for dr in &dirty_rects_since_refresh {
                            #[cfg(feature = "eink_device")]
                            {
                                fb.update(&dr, UpdateMode::Full).ok();
                            }
                        }
                        dirty_update_count = 0;
                        dirty_rects_since_refresh.clear();
                    } else {
                        for dr in &dirty_rects {
                            debug!("Updating dirty rect {:?}", dr);

                            #[cfg(feature = "eink_device")]
                            {
                                if dr.height() < 100 && dr.width() < 100 {
                                    debug!("Fast mono update!");
                                    fb.update(&dr, UpdateMode::FastMono).ok();
                                } else {
                                    fb.update(&dr, UpdateMode::Partial).ok();
                                }
                            }

                            push_to_dirty_rect_list(&mut dirty_rects_since_refresh, *dr);
                        }

                        time_at_last_draw = Instant::now();
                    }

                    dirty_rects.clear();

                    frame_complete = true;
                }
                // x => info!("{:?}", x), /* ignore unsupported events */
                _ => (),
            }
        }

        if frame_complete {
            if vnc.request_update(
                Rect { left: 0, top: 0, width, height },
                true,
            ).is_err() {
                error!("server disconnected");
                break;
            }
        }

        if FRAME_MS > time_at_sol.elapsed().as_millis() as u64 {
            if dirty_rects_since_refresh.len() > 0 && time_at_last_draw.elapsed().as_secs() > 3 {
                for dr in &dirty_rects_since_refresh {
                    #[cfg(feature = "eink_device")]
                    {
                        fb.update(&dr, UpdateMode::Full).ok();
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
