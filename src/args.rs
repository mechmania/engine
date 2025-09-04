use std::path::PathBuf;
use std::fs::File;
use std::io::{ self, BufWriter, Write };
use tokio::sync::mpsc;
use clap::Parser;


#[derive(Parser, Clone)]
#[command(version, about, long_about = None)]
pub struct ArgConfig {
    /// path to bot a binary
    pub bot_a: PathBuf,
    /// path to bot b binary
    pub bot_b: PathBuf,
    /// output sources to print (e.g., -p a,b,g)
    #[arg(short = 'p', long = "print", value_delimiter = ',', value_parser = parse_source)]
    pub print: Option<Vec<OutputSource>>,
    /// output sources redirected to file, format: a:foo.txt g:log.json
    #[arg(short = 'o', long = "output", value_parser = parse_output_mappings)]
    pub output: Option<Vec<OutputMapping>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OutputSource {
    BotA,
    BotB,
    Gamelog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputMapping {
    pub sources: Vec<OutputSource>,
    pub path: PathBuf,
}

pub struct Message {
    pub msg: String,
    pub source: OutputSource
}

pub fn parse_cli() -> ArgConfig { 
    let mut cli = ArgConfig::parse();
    if let (None, None) = (cli.print.as_ref(), cli.output.as_ref()) {
        cli.print = Some(vec![
            OutputSource::BotA,
            OutputSource::BotB,
            OutputSource::Gamelog,
        ]);
    }
    cli
}

fn parse_output_mappings(s: &str) -> Result<OutputMapping, String> {
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid format for output mapping '{}'. Use -o a,b:foo.txt",
            s
        ));
    }
    let sources: Vec<OutputSource> = parts[0]
        .split(',')
        .map(parse_source)
        .collect::<Result<Vec<_>, _>>()?;
    let path = PathBuf::from(parts[1]);
    Ok(OutputMapping { sources, path })
}

fn parse_source(s: &str) -> Result<OutputSource, String> {
     match s {
        "a" | "A" => Ok(OutputSource::BotA),
        "b" | "B" => Ok(OutputSource::BotB),
        "g" | "G" => Ok(OutputSource::Gamelog),
        _ => Err(format!("Invalid source '{}'", s)),
    }
}

struct OutputConfig {
    files: Box<[BufWriter<File>]>,

    print: [bool; 3],
    output_files: [Box<[u8]>; 3],
}

impl OutputConfig {
    fn send(&mut self, msg: Message) -> io::Result<()> {
        let idx = msg.source as usize;
        if self.print[idx] {
            println!("{}", msg.msg);
        }
        for i in &self.output_files[idx] {
            writeln!(self.files[*i as usize], "{}", msg.msg)?;
        }
        Ok(())
    }
}


pub use crate::send;
#[macro_export]
macro_rules! send {
    ($tx:expr, $source:expr, $fmt:literal $(, $($args:expr),*)?) => {
        {
            let _ = $tx.send(Message {
                msg: format!($fmt $(, $($args),*)?),
                source: $source
            });
        }
    };
}

pub fn spawn_reciever(cli: &ArgConfig) -> io::Result<(mpsc::UnboundedSender<Message>, tokio::task::JoinHandle<io::Result<()>>)> {
    let (tx, mut rx) = mpsc::unbounded_channel();

    let mut print = [false; 3];

    if let Some(prints) = &cli.print {
        for p in prints {
            print[*p as usize] = true;
        }
    }

    let mut files: Vec<BufWriter<File>> = vec![];
    let mut output_files: [Vec<u8>; 3] = core::array::from_fn(|_| vec![]);

    if let Some(output) = &cli.output {
        for (i, o) in output.iter().enumerate() {
            let buf = BufWriter::new(File::create(&o.path)?);
            files.push(buf);
            for s in &o.sources {
                output_files[*s as usize].push(i as u8);
            }
        }
    }

    let mut conf = OutputConfig {
        files: files.into(),
        print,
        output_files: output_files.map(Vec::into_boxed_slice)
    };

    let task = tokio::task::spawn(async move {
        while let Some(msg) = rx.recv().await {
            conf.send(msg)?;
        }
        Ok(())
    });

    Ok((tx, task))
}
