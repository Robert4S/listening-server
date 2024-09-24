use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use regex::Regex;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{OnceCell, RwLock};
use tokio::{spawn, time};
use tracing::{debug, info};

use clap::Parser;

use local_ip_address::local_ip;

#[derive(Parser)]
struct Config {
    file: String,
}

type Student = String;

#[derive(Debug)]
struct Classroom {
    students: Vec<Student>,
}

impl Classroom {
    fn to_string(&self) -> String {
        self.students.iter().cloned().collect()
    }
}

static STUDENTS: OnceCell<RwLock<Classroom>> = OnceCell::const_new();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    STUDENTS
        .get_or_init(move || async move {
            RwLock::new(Classroom {
                students: Vec::new(),
            })
        })
        .await;

    let Config { file } = Config::parse();

    let ip = local_ip().expect("Should be able to get local ip");
    println!("{ip}");
    let code = ip.to_string();

    let should_play = Arc::new(AtomicBool::new(false));
    let chclone = should_play.clone();
    let input_handle = tokio::spawn(async move {
        let res = when_to_start_udp(chclone).await;
        if let Err(e) = res {
            eprintln!("Something whent wrong {e:?}");
        }
    });

    let chclone = should_play.clone();
    let data_handle = tokio::spawn(async move {
        let res = send_data(chclone, file, code).await;
        match res {
            Ok(_) => {}
            Err(e) => {
                eprintln!("{e}");
            }
        }
    });
    input_handle.await?;
    data_handle.await?;

    Ok(())
}

async fn send_data(
    start_signal: Arc<AtomicBool>,
    file: String,
    code: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    info!("Listening for audio data at {code}:8080");
    loop {
        let (socket, _) = match listener.accept().await {
            Ok(o) => o,
            Err(_) => {
                panic!();
            }
        };
        let sockname = socket.peer_addr()?;
        let file = file.clone();
        let code = code.clone();
        info!("Connection started at {sockname}");
        let start_signal = start_signal.clone();
        tokio::spawn(async move {
            match handle_sender(socket, start_signal, file, code).await {
                Ok(_) => debug!("Connection succesfully handled at {sockname:?}"),
                Err(e) => debug!("Error {e} at {sockname:?}"),
            }
        });
    }
}

async fn handle_sender(
    mut socket: TcpStream,
    start_signal: Arc<AtomicBool>,
    file: String,
    code: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::open(&file).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;

    let mut buf = [0; 20];
    loop {
        let bytes_read = socket.read(&mut buf).await?;
        if bytes_read == 0 {
            continue;
        }

        let input = std::str::from_utf8(&buf[..bytes_read])?.trim().to_string();

        if input.as_str() == &code {
            socket.write_all(&[1]).await?;
            break;
        }
        socket.write_all(&[0]).await?;
        buf = [0; 20];
    }

    buf = [0; 20];

    let mut name_buf = [0; 100];
    loop {
        let bytes_read = socket.read(&mut name_buf).await?;
        if bytes_read == 0 {
            continue;
        }

        let name = std::str::from_utf8(&name_buf[..bytes_read])?
            .trim()
            .to_string();

        STUDENTS
            .get()
            .ok_or(Error)?
            .write()
            .await
            .students
            .push(name);

        break;
    }

    loop {
        let bytes_read = socket.read(&mut buf).await?;
        if bytes_read == 0 {
            continue;
        }

        let input = std::str::from_utf8(&buf[..bytes_read])?.trim().to_string();

        if input.as_str() == "send" {
            let data_len = buffer.len() as u64;
            let len_bytes = data_len.to_be_bytes();

            socket.write_all(&len_bytes).await?;

            socket.write_all(&buffer).await?;
            break;
        }
    }

    loop {
        let ch_out: bool;
        {
            ch_out = start_signal.load(Ordering::SeqCst);
        }
        if ch_out {
            socket.write_all(&[1]).await?;
        } else {
            socket.write_all(&[0]).await?;
        }
        tokio::time::sleep(time::Duration::from_micros(50)).await;
    }
}

async fn when_to_start_udp(
    start_signal: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:3000").await?);
    info!("Listening for start/stop signals at 127.0.0.1:3000");
    let mut buffer = [0; 10];
    {
        let start_signal = start_signal.clone();
        spawn(async move {
            match handle_starter_udp(start_signal).await {
                Err(e) => println!("i got {e}"),
                _ => {}
            }
        });
    }

    loop {
        let (bytes_read, _) = socket.recv_from(&mut buffer).await?;
        if bytes_read == 0 {
            continue;
        }

        let input = std::str::from_utf8(&buffer[..bytes_read])?
            .trim()
            .to_string();

        let skip_regex = Regex::new(r"scrub \d+(\.\d+)?").unwrap();

        match input.as_str().trim() {
            "start" => {
                start_signal.store(true, Ordering::SeqCst);
                eprintln!("Start signal recieved");
            }
            "stop" => {
                start_signal.store(false, Ordering::SeqCst);
                eprintln!("Stop singal recieved");
            }
            other => {}
        }
    }
}

async fn handle_starter_udp(
    start_signal: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:4565").await?);

    loop {
        let students = STUDENTS
            .get()
            .ok_or(Error)?
            .read()
            .await
            .to_string()
            .as_bytes()
            .to_vec();

        let mut sent = [0; 1];
        let (_, sender) = socket.recv_from(&mut sent).await?;
        let should_play = start_signal.load(Ordering::SeqCst);

        let combined: Vec<u8> = vec![
            match should_play {
                true => vec![1],
                false => vec![0],
            },
            students,
        ]
        .iter()
        .flatten()
        .map(|b| *b)
        .collect();

        let _ = socket.send_to(&combined, &sender).await?;
    }
}

#[derive(Debug)]
struct Error;

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(feature = "deprecated")]
async fn when_to_start(
    start_signal: Arc<RwLock<AtomicBool>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:3000").await?;
    info!("Listening for start/stop signals at 127.0.0.1:3000");

    loop {
        let (socket, _) = listener.accept().await?;
        let sockname = socket.peer_addr()?;
        info!("Connection started at {sockname}");
        let start_signal = start_signal.clone();
        tokio::spawn(async move {
            match handle_starter(start_signal, socket).await {
                Ok(_) => debug!("Connection succesfully handled at {sockname:?}"),
                Err(e) => debug!("Error {e} at {sockname:?}"),
            }
        });
    }
}

#[cfg(feature = "deprecated")]
async fn handle_starter(
    start_signal: Arc<RwLock<AtomicBool>>,
    mut socket: TcpStream,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = [0; 10];
    loop {
        let bytes_read = socket.read(&mut buffer).await?;
        if bytes_read == 0 {
            continue;
        }

        let input = std::str::from_utf8(&buffer[..bytes_read])?
            .trim()
            .to_string();

        match input.as_str() {
            "start" => {
                let should_play = start_signal.write().unwrap();
                should_play.store(true, Ordering::SeqCst);
                eprintln!("Start signal recieved");
            }
            "stop" => {
                let should_play = start_signal.write().unwrap();
                should_play.store(false, Ordering::SeqCst);
                eprintln!("Stop singal recieved");
            }
            _ => {}
        }
    }
}
