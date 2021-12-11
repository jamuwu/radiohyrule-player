#![windows_subsystem = "windows"]

use std::io::prelude::*;
use std::net::TcpStream;
use std::thread;
use std::sync::RwLock;

use chrono::prelude::*;
use crossbeam::channel::{
  Sender,
  TrySendError,
  TryRecvError,
  unbounded,
};
use reqwest::blocking::Client;
use serde::Deserialize;

use sdl2::audio::AudioSpecDesired;
use sdl2::event::Event;
use sdl2::gfx::framerate::FPSManager;
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::image::{InitFlag, LoadTexture};
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sdl2::rect::Rect;
use sdl2::pixels::Color;
use sdl2::render::TextureQuery;
use sdl2::surface::Surface;
use sdl2::video::WindowPos;

use winapi::um::winuser::GetCursorPos;
use winapi::shared::windef::POINT;

const DEFAULT: &[u8; 13474] = include_bytes!("default.png");
const FONT: &[u8; 265612] = include_bytes!("SourceSansPro-Black.ttf");

lazy_static::lazy_static! {
  static ref OFFSET: RwLock<i64> = RwLock::new(0);
  static ref PLAYING: RwLock<bool> = RwLock::new(true);
}

fn main() -> Result<(), MyError> {
  let sdl_context = sdl2::init()?;
  let video_subsystem = sdl_context.video()?;
  let audio_subsystem = sdl_context.audio()?;
  sdl2::image::init(InitFlag::PNG | InitFlag::JPG)?;
  let ttf_context = sdl2::ttf::init().map_err(|e| e.to_string())?;

  #[cfg(debug_assertions)]
  let mut window = video_subsystem.window("Radio Hyrule", 600, 200)
    .build().map_err(|e| e.to_string())?;
  
  #[cfg(not(debug_assertions))]
  let mut window = video_subsystem.window("Radio Hyrule", 600, 200)
    .borderless().build().map_err(|e| e.to_string())?;

  let mut icon: Vec<u8> = include_bytes!("icon.raw").to_vec();
  let icon = Surface::from_data(&mut icon, 64, 64, 256, sdl2::pixels::PixelFormatEnum::ABGR8888)?;
  window.set_icon(&icon);

  let mut canvas = window
    .into_canvas()
    .software()
    .build()
    .map_err(|e| e.to_string())?;
  canvas.present();
  let texture_creator = canvas.texture_creator();
  let mut current_texture = DEFAULT.to_vec();

  let font = ttf_context.load_font_from_rwops(
    sdl2::rwops::RWops::from_bytes(FONT)?, 32
  )?;
  let small_font = ttf_context.load_font_from_rwops(
    sdl2::rwops::RWops::from_bytes(FONT)?, 16
  )?;

  let (songs, updater) = unbounded::<SongUpdate>();
  thread::spawn(move || {
    song_updater(songs)
  });

  let (sender, audio) = unbounded::<Vec<i16>>();
  thread::spawn(|| {
    radio(sender)
  });

  let desired_spec = AudioSpecDesired {
    freq: Some(44_100),
    channels: Some(2),
    samples: None,
  };
  let device = audio_subsystem.open_queue::<i16, _>(None, &desired_spec)?;
  device.resume();

  let mut current = NowPlaying::default();

  let mut events = sdl_context.event_pump().map_err(|e| e.to_string())?;
  let mut dragging = None;

  let mut fps = FPSManager::new();
  // Don't worry, your resources aren't actually being used much,
  // Since only moving the window is using this framerate,
  // Everything is drawn at most once every 100ms.
  fps.set_framerate(120)?;

  let mut time_since_last_draw = 0;

  'running: loop {
    for event in events.poll_iter() {
      match event {
        Event::Quit { .. }
        | Event::KeyDown {
          keycode: Some(Keycode::Escape),
          ..
        } => break 'running,
        Event::MouseButtonDown { x, y, mouse_btn: MouseButton::Left, .. } => {
          if x >= 580 && x <= 600 && y >= 0 && y <= 20 {
            break 'running
          } else if x >= 200 && x <= 210 && y >= 190 && y <= 200 {
            let mut playing = PLAYING.write().unwrap();
            if *playing {
              device.pause();
              *playing = false;
              device.clear();
            } else {
              device.resume();
              *playing = true;
            }
          }
        },
        Event::MouseButtonUp { mouse_btn: MouseButton::Left, .. } => {
          dragging = None;
        },
        Event::KeyDown { keycode: Some(Keycode::Space), .. } => {
          let mut playing = PLAYING.write().unwrap();
          if *playing {
            device.pause();
            *playing = false;
            device.clear();
          } else {
            device.resume();
            *playing = true;
          }
        },
        _ => {}
      }
    }

    if events.mouse_state().is_mouse_button_pressed(MouseButton::Left) {
      if let None = dragging {
        dragging = Some(events.mouse_state());
      }
      let mut point = POINT { x: 0, y: 0 };
      // Should always work, but just in case
      if unsafe { GetCursorPos(&mut point) } != 0 {
        let x = point.x - dragging.unwrap().x();
        let y = point.y - dragging.unwrap().y();
        canvas.window_mut().set_position(WindowPos::Positioned(x), WindowPos::Positioned(y));
      }
    }

    for frame in audio.try_iter() {
      device.queue(&frame);
    }

    if time_since_last_draw >= 100 {
      let queue_size = device.size() as i32;
      let window = canvas.window_mut();
      let title = if queue_size <= 2048 {
        format!("Radio Hyrule ({})", queue_size)
      } else if queue_size <= 2097152 { // 2048 * 1024
        format!("Radio Hyrule ({}KB)", queue_size / 1024)
      } else {
        format!("Radio Hyrule ({:2}MB)", queue_size as f32 / 1048576.0)
      };
      window.set_title(&title).map_err(|e| e.to_string())?;
  
      canvas.set_draw_color(Color::RGB(0, 0, 0));
      canvas.clear();
  
      canvas.set_draw_color(Color::RGB(123, 123, 123));
      match updater.try_recv() {
        Ok(song) => {
          current = song.json.clone();
          println!("Received: {:?}", song.json);
          current_texture = match song.cover {
            Some(bytes) => bytes,
            None => DEFAULT.to_vec(),
          };
        },
        Err(TryRecvError::Empty) => {},
        _ => break,
      }
      let texture = texture_creator.load_texture_bytes(&current_texture)?;
      canvas.copy(&texture, None, Rect::new(0, 0, 200, 200))?;
  
      let surface = font
        .render(if current.title.len() > 0 { &current.title } else { "Nothing playing" })
        .blended_wrapped(Color::RGB(255, 255, 255), 350)
        .map_err(|e| e.to_string())?;
      let title = texture_creator
        .create_texture_from_surface(&surface)
        .map_err(|e| e.to_string())?;
      let title_query = title.query();
      canvas.copy(&title, None, Rect::new(225, 15, title_query.width, title_query.height))?;
      if let Some(artist) = &current.artist {
        if artist.len() > 0 {
          let surface = small_font
            .render(&format!("By {}", artist[0]))
            .blended(Color::RGB(200, 200, 200))
            .map_err(|e| e.to_string())?;
          let artist = texture_creator
            .create_texture_from_surface(&surface)
            .map_err(|e| e.to_string())?;
          let artist_query = artist.query();
          canvas.copy(&artist, None, Rect::new(
            225, 15 + title_query.height as i32,
            artist_query.width, artist_query.height,
          ))?;
        }
      }
  
      let surface = small_font
        .render(&format!("{} listeners", current.listeners))
        .blended(Color::RGB(78, 78, 78))
        .map_err(|e| e.to_string())?;
      let title = texture_creator
        .create_texture_from_surface(&surface)
        .map_err(|e| e.to_string())?;
      let TextureQuery { width, height, .. } = title.query();
      canvas.copy(&title, None, Rect::new(225, 160, width, height))?;
    
      canvas.set_draw_color(Color::RGB(255, 128, 128));
      canvas.fill_rect(Rect::new(581, 1, 18, 18))?;
      if *PLAYING.read().unwrap() {
        canvas.box_(201, 190, 204, 200, Color::RGB(200, 200, 200))?;
        canvas.box_(206, 190, 209, 200, Color::RGB(200, 200, 200))?;
      } else {
        canvas.filled_trigon(202, 191, 202, 199, 208, 195, Color::RGB(200, 200, 200))?;
      }
      canvas.box_(211, 190, 600, 200, Color::RGB(128, 128, 128))?;
      if let Some(duration) = current.duration {
        let offset = *OFFSET.read().unwrap();
        let end = current.started + duration as i64;
        let length = end - current.started;
        let now = Utc::now().timestamp_millis() / 1000;
        let progress = (now - current.started - offset) as f64 / length as f64;
        canvas.box_(211, 190, 211 + (progress * 390.0) as i16, 200, Color::RGB(225, 180, 36))?;
      }
      canvas.present();
      time_since_last_draw = 0;
    } else {
      time_since_last_draw += fps.delay();
    }
  }

  Ok(())
}

fn radio(sender: Sender<Vec<i16>>) -> Result<(), MyError> {
  let mut stream = TcpStream::connect("radiohyrule.com:8000")?;
  stream.write(b"GET /listen HTTP/1.1\r\n\r\n")?;

  let mut buf = Vec::new();
  let mut byte = [0u8; 1];

  while !buf.ends_with(b"\r\n\r\n") {
    stream.read_exact(&mut byte)?;
    buf.push(byte[0]);
  }

  if &buf[..17] != b"HTTP/1.0 200 OK\r\n" {
    return Err(MyError { details: String::from("Stream not found") })
  }

  let mut mp3 = minimp3::Decoder::new(stream);

  loop {    
    if let Ok(frame) = mp3.next_frame() {
      let data: Vec<i16> = frame.data.iter().map(|x| (*x as f32 * 0.07) as i16).collect();
      if *PLAYING.read().unwrap() {
        match sender.try_send(data) {
          Err(TrySendError::Disconnected(_)) => break,
          _ => {},
        }
      }
    }
  }

  Ok(())
}

fn song_updater(sender: Sender<SongUpdate>) -> Result<(), MyError> {
  let client = Client::new();
  let url = "https://radiohyrule.com/nowplaying.json";
  let now = Utc::now().timestamp_millis();
  let res = client.get(url).send()?;
  *OFFSET.write().unwrap() = if let Some(date) = res.headers().get("Date") {
    let server = DateTime::parse_from_rfc2822(date.to_str().unwrap())?.timestamp_millis();
    (now - server) / 1000
  } else {
    0
  };
  let mut current: NowPlaying = res.json()?;
  current.info();
  let cover = match current.albumcover.clone() {
    Some(name) => {
      let res = client.get(&format!(
        "https://radiohyrule.com/albumart/cover320/{}", name
      )).send()?;
      Some(res.bytes()?.to_vec())
    },
    None => None,
  };
  sender.try_send(SongUpdate { json: current.clone(), cover: cover })?;
  let mut previous = current.title.clone();
  let mut next_epoch = current.started + (current.duration.unwrap_or(1.0) * 1000.0) as i64 - *OFFSET.read().unwrap();
  let three = std::time::Duration::from_secs(3);
  loop {
    let now = Utc::now().timestamp_millis();
    if now > next_epoch {
      let res = client.get(&format!("{}?_={}", url, now)).send()?;
      current = res.json()?;
      if &current.title != &previous {
        current.info();
        let cover = match current.albumcover.clone() {
          Some(name) => {
            let res = client.get(&format!(
              "https://radiohyrule.com/albumart/cover320/{}", name
            )).send()?;
            Some(res.bytes()?.to_vec())
          },
          None => None,
        };
        next_epoch = current.started + (current.duration.unwrap_or(1.0) * 1000.0) as i64 - *OFFSET.read().unwrap();
        match sender.try_send(SongUpdate { json: current.clone(), cover: cover }) {
          Err(TrySendError::Disconnected(_)) => break,
          _ => {},
        }
      }
    }
    previous = current.title.clone();
    thread::sleep(three);
  }

  Ok(())
}

#[derive(Clone, Debug)]
struct SongUpdate{
  json: NowPlaying,
  cover: Option<Vec<u8>>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, Deserialize)]
struct NowPlaying {
  album: Option<String>,
  album_url: Option<String>,
  albumcover: Option<String>,
  artist: Option<Vec<String>>,
  artist_url: Option<Vec<String>>,
  duration: Option<f32>,
  listeners: usize,
  song_nid: Option<String>,
  song_url: Option<String>,
  source: Option<String>,
  started: i64,
  title: String,
}

impl NowPlaying {
  fn info(&self) {
    if let Some(artist) = &self.artist {
      println!("{} - {} with {} listeners", artist[0], self.title, self.listeners)
    } else {
      println!("{} with {} listeners", self.title, self.listeners)
    }
  }
}

#[derive(Debug)]
struct MyError {
  details: String
}

impl std::fmt::Display for MyError {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f,"{}",self.details)
  }
}

impl std::error::Error for MyError {
  fn description(&self) -> &str {
    &self.details
  }
}

impl From<String> for MyError {
  fn from(err: String) -> MyError {
    MyError {
      details: err,
    }
  }
}

impl From<std::io::Error> for MyError {
  fn from(err: std::io::Error) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}

impl From<chrono::ParseError> for MyError {
  fn from(err: chrono::ParseError) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}

impl From<reqwest::Error> for MyError {
  fn from(err: reqwest::Error) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}

impl From<minimp3::Error> for MyError {
  fn from(err: minimp3::Error) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}

impl From<sdl2::render::UpdateTextureError> for MyError {
  fn from(err: sdl2::render::UpdateTextureError) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}

impl From<TrySendError<SongUpdate>> for MyError {
  fn from(err: TrySendError<SongUpdate>) -> MyError {
    MyError {
      details: err.to_string(),
    }
  }
}