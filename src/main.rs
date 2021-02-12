use ansi_term::Style;
use chrono::DateTime;
use configparser::ini::Ini;
use getopts::Options;
use skim::prelude::*;
use std::env;
use std::fs::File;
use std::io::{ Read, Write };
use std::path::Path;
use std::process::{ Command, Stdio };
use std::sync::{ Arc, Mutex, Once, mpsc };
use std::time::Duration;

#[derive( Clone )]
enum UeberzugAction {
	Add( String, usize ),
	Remove,
	Exit
}

lazy_static::lazy_static! {
	static ref HOME_DIR : String = String::from( home::home_dir().unwrap_or( env::current_dir().unwrap() ).to_str().unwrap() );
	static ref CACHE_DIR : String = format!( "{}/.cache/yt-cli", *HOME_DIR );
	static ref UEBERZUG_INIT : Once = Once::new();
	static ref UEBERZUG_TX : Mutex<Option<mpsc::Sender<UeberzugAction>>> = Mutex::new( None );
}

pub struct YTCli {
	config : Ini
}

impl YTCli {
	pub fn new( path : String ) -> YTCli {
		let mut config = Ini::new_cs();
		let configpath = Path::new( &path );

		if configpath.exists() {
			config.load( &path ).expect( "Failed to load config" );
		} else {
			println!( "{}", Style::from( ansi_term::Color::Red ).bold().paint( format!( "Create a config file ({}) first!", path ) ) );
		}

		std::fs::create_dir_all( &*CACHE_DIR ).expect( "Failed to create cache directory" );
		std::fs::create_dir_all( format!( "{}/{}", *CACHE_DIR, "feed" ) ).expect( "Failed to create cache directory" );
		std::fs::create_dir_all( format!( "{}/{}", *CACHE_DIR, "thumb" ) ).expect( "Failed to create cache directory" );

		YTCli {
			config
		}
	}

	fn topics( &self, filter : String ) -> Vec<YTTopic> {
		let map = self.config.get_map().unwrap_or_default();

		let allowed = filter
			.split( | c | {
				char::is_whitespace( c ) || c == ',' || c == ';'
			} )
			.map( | t | {
				String::from( t )
			} )
			.collect::<String>();

		map
			.keys()
			.filter_map( | topic : &String | -> Option<YTTopic> {
				let section = map.get( topic );

				if topic == "default" || ( allowed.len() > 0 && !allowed.contains( topic ) ) {
					None
				} else {
					Some( YTTopic {
						name: topic.clone(),
						channels: match section {
							Some( v ) => {
								v.keys().map( | channel : &String | -> YTChannel {
									if v[channel].is_some() {
										YTChannel {
											id: v[channel].clone().unwrap(),
											name: Some( channel.clone() )
										}
									} else {
										YTChannel {
											id: channel.clone(),
											name: None
										}
									}
								} ).collect()
							}
							None => { Vec::new() }
						}
					} )
				}
			} )
			.collect()
	}

	fn skim( &self, feed : &YTFeed ) -> Vec<Arc<dyn SkimItem>> {
		let options = SkimOptionsBuilder::default()
			.height( Some( "100%" ) )
			.multi( true )
			.preview( Some( "" ) )
			.preview_window( Some( "right:wrap" ) )
			.build()
			.unwrap();

		let ( stx, srx ) : ( SkimItemSender, SkimItemReceiver ) = unbounded();

		for video in feed.videos.clone() {
			stx.send( Arc::new( video ) ).expect( "Failed writing to skim" );
		}
		drop( stx );

		Skim::run_with( &options, Some( srx ) )
			.map( | out | match out.final_event {
				Event::EvActAccept( _ ) => out.selected_items,
				_ => Vec::new()
			} )
			.unwrap_or_else( || Vec::new() )
	}

	fn ueberzug() {
		let ueberzug = Command::new( "ueberzug" )
			.arg( "layer" )
			.arg( "--silent" )
			.stdin( Stdio::piped() )
			.spawn()
			.expect( "Failed to start ueberzug" );

		let mut ueberzugin = ueberzug.stdin.expect( "Failed to open ueberzug stdin" );

		let ( utx, urx ) = mpsc::channel::<UeberzugAction>();

		let mut mutx = UEBERZUG_TX.lock().expect( "Failed to lock UEBERZUG_TX" );
		*mutx = Some( utx );

		std::thread::spawn( move || {
			let mut lastaction = None;
			let trap = signal::trap::Trap::trap( &[ signal::Signal::SIGWINCH ] );

			loop {
				if let Ok( v ) = urx.recv_timeout( Duration::from_micros( 1 ) ) {
					lastaction = Some( v.clone() );
					match v {
						UeberzugAction::Add( path, offset ) => writeln!(
							ueberzugin,
							"{{\"action\":\"add\",\"identifier\":\"preview\",\"path\":\"{}/thumb/{}.jpg\",\"x\":{},\"y\":0,\"width\":{},\"scaler\":\"contain\",\"scaling_position_x\":0.5,\"scaling_position_y\":0.5}}",
							*CACHE_DIR,
							path,
							offset + 3,
							offset
						),
						UeberzugAction::Remove => writeln!( ueberzugin, "{{\"action\":\"remove\",\"identifier\":\"preview\"}}" ),
						UeberzugAction::Exit => break
					}.unwrap();
				}

				for _ in trap.wait( std::time::Instant::now() ) {
					if lastaction.is_some() {
						match lastaction.clone().unwrap() {
							UeberzugAction::Add( path, _ ) => {
								let offset = String::from_utf8_lossy(
									&Command::new( "tput" )
										.arg( "cols" )
										.stdout( Stdio::piped() )
										.output()
										.expect( "Failed to start tput" )
										.stdout
								)
									.trim()
									.parse::<u32>()
									.unwrap_or( 0 );

								writeln!(
									ueberzugin,
									"{{\"action\":\"add\",\"identifier\":\"preview\",\"path\":\"{}/thumb/{}.jpg\",\"x\":{},\"y\":0,\"width\":{},\"scaler\":\"contain\",\"scaling_position_x\":0.5,\"scaling_position_y\":0.5}}",
									*CACHE_DIR,
									path,
									offset / 2 + 1,
									offset / 2
								).unwrap_or_default();
							}
							_ => {}
						}
					}
				}

				std::thread::yield_now();
			};
		} );
	}
}

struct YTFeed {
	videos : Vec<YTVideo>
}

impl YTFeed {
	fn from_channels( channels : Vec<YTChannel> ) -> YTFeed {
		let mut videos = Vec::new();
		let mut tasks = Vec::new();

		for channel in channels {
			tasks.push( std::thread::spawn( move || {
				channel.videos()
			} ) );
		}

		for task in tasks {
			videos.append( task.join().as_mut().unwrap_or( &mut Vec::new() ) );
		}

		videos.sort_by_key( | e | { e.timestamp } );
		videos.reverse();

		YTFeed {
			videos: videos
		}
	}

	fn from_topics( topics : impl IntoIterator<Item = YTTopic> ) -> YTFeed {
		YTFeed::from_channels( topics.into_iter().flat_map( | t | -> Vec<YTChannel> { t.channels } ).collect() )
	}
}

#[derive( Clone )]
struct YTTopic {
	name : String,
	channels : Vec<YTChannel>
}

#[derive( Clone )]
struct YTChannel {
	id : String,
	name : Option<String>
}

impl YTChannel {
	fn name( &self ) -> Option<String> {
		if self.name.is_some() {
			return self.name.clone();
		}

		let pathstr = format!( "{}/feed/{}.json", *CACHE_DIR, self.id );
		let path = Path::new( &pathstr );

		if path.exists() {
			let mut feedraw = String::new();
			let mut file = File::open( &path ).expect( "Failed to open file" );
			file.read_to_string( &mut feedraw ).expect( "Failed to read xq results" );
			let feed = json::parse( &feedraw ).expect( "Invalid JSON provided" );

			if feed["feed"].members().len() > 0 {
				let author = feed["feed"][0]["author"].as_str();

				if author.is_some() {
					return Some( String::from( author.unwrap_or_default() ) );
				}
			}
		}

		None
	}

	fn videos( &self ) -> Vec<YTVideo> {
		let pathstr = format!( "{}/feed/{}.json", *CACHE_DIR, self.id );
		let path = Path::new( &pathstr );

		if !path.exists() || path.metadata().expect( "Failed to retreive cache metadata" ).modified().unwrap().elapsed().unwrap() > Duration::from_secs( 1800 ) {
			let res = reqwest::blocking::get( &format!( "https://www.youtube.com/feeds/videos.xml?channel_id={}", self.id ) ).unwrap();
			let file = File::create( &path ).expect( "Failed to create file" );

			let mut xq = Command::new( "xq" )
				.arg( "{ FEEDVERSION: 1, feed: [ .feed.entry[] | { id: .[\"yt:videoId\"], title: .title, author: .author.name, description: .[\"media:group\"][\"media:description\"], timestamp: .published } ] }" )
				.stdin( Stdio::piped() )
				.stdout( Stdio::from( file ) )
				.spawn()
				.expect( "xq failed to start" );

			xq.stdin
				.take()
				.expect( "Failed to open xq's stdin" )
				.write( &res.bytes().expect( "Failed to retreive request content" ) )
				.expect( "Failed to write to xq's stdin" );

			xq.wait().expect( "xq failed" );
		}

		let mut feedraw = String::new();
		let mut file = File::open( &path ).expect( "Failed to open file" );
		file.read_to_string( &mut feedraw ).expect( "Failed to read xq results" );

		let feed = json::parse( &feedraw ).expect( "Invalid JSON provided" );
		let mut out : Vec<YTVideo> = Vec::new();

		for video in feed["feed"].members() {
			out.push( YTVideo {
				id: String::from( video["id"].as_str().expect( "Invalid JSON provided" ) ),
				author: String::from( video["author"].as_str().expect( "Invalid JSON provided" ) ),
				title: String::from( video["title"].as_str().expect( "Invalid JSON provided" ) ),
				description: String::from( video["description"].as_str().expect( "Invalid JSON provided" ) ),
				timestamp: DateTime::parse_from_rfc3339(
					video["timestamp"].as_str().expect( "Invalid JSON provided" )
				).expect( "Invalid JSON provided" ).with_timezone( &chrono::Local.clone() )
			} )
		}

		out
	}
}

#[derive( Clone )]
struct YTVideo {
	id : String,
	title : String,
	author : String,
	description : String,
	timestamp : DateTime<chrono::Local>
}

impl YTVideo {
	fn url( &self ) -> String {
		format!( "https://youtube.com/watch?v={}", self.id )
	}

	fn thumbnail( &self, width : usize ) {
		UEBERZUG_INIT.call_once( YTCli::ueberzug );

		let pathstr = format!( "{}/thumb/{}.jpg", *CACHE_DIR, self.id );
		let path = Path::new( &pathstr );
		let utx = UEBERZUG_TX.lock().expect( "Failed to lock UEBERZUG_TX" );

		if !path.exists() {
			utx
				.as_ref()
				.unwrap()
				.send( UeberzugAction::Remove )
				.unwrap();

			let res = reqwest::blocking::get( &format!( "https://i.ytimg.com/vi/{}/hq720.jpg", self.id ) ).unwrap();
			let mut file = File::create( &path ).expect( "Failed to create file" );

			file.write( &res.bytes().unwrap() ).unwrap();
		}

		utx
			.as_ref()
			.unwrap()
			.send( UeberzugAction::Add( self.id.clone(), width ) )
			.unwrap();
	}
}

impl SkimItem for YTVideo {
	fn text( &self ) -> Cow<str> {
		Cow::Owned( self.to_string() )
	}

	fn preview( &self, context: PreviewContext ) -> ItemPreview {
		let bold = Style::new().bold();

		let s = self.clone();
		let w = context.width;
		std::thread::spawn( move || {
			s.thumbnail( w );
		} );

        ItemPreview::AnsiText(
			format!(
				"{}{}\n{} | {}\n\n{}",
				( 0..=( context.width / ( 1280 / 720 ) / 4 ) ).map( |_| "\n" ).collect::<String>(),
				bold.paint( self.title.clone() ),
				bold.paint( self.author.clone() ),
				self.timestamp.format( "%Y-%m-%d %H:%M:%S" ),
				self.description
			)
		)
	}
}

impl ToString for YTVideo {
	fn to_string( &self ) -> String {
		format!( "[{}] {}", self.author, self.title )
	}
}

fn main() {
	let ytcli = YTCli::new( format!( "{}/.config/yt-cli.cfg", *HOME_DIR ) );
	let bold = Style::new().bold();

	let args : Vec<String> = env::args().collect();
	let mut opts = Options::new();
	opts.optflag( "h", "help", "shows help message" );
	opts.optflag( "l", "list-channels", "lists subscribed channels" );
	opts.optflag( "L", "list-topics", "lists subscribed topics" );
	opts.optopt( "t", "topics", "show videos only from listed TOPICS", "TOPICS" );

	let matches = match opts.parse( &args[1..] ) {
		Ok( m ) => { m }
		Err( f ) => { println!( "Error: {}", f.to_string() ); return }
	};

	if matches.opt_present( "h" ) {
		print!( "{}", opts.usage( "yt-cli (https://github.com/lkucharczyk/yt-cli)" ) );
		return;
	} else if matches.opt_present( "l" ) {
		println!( "{}", bold.paint( "Subscribed channels: " ) );

		let mut topics = ytcli.topics( matches.opt_str( "t" ).unwrap_or_default() );
		topics.sort_by_cached_key( | t | { t.name.clone() } );

		for topic in topics {
			println!( "{}", topic.name );

			let mut channels = topic.channels;
			channels.sort_by_cached_key( | c | { c.name().unwrap_or( "~".to_string() + &c.id ) } );

			for channel in channels {
				let name = channel.name();

				if name.is_some() {
					println!( "  {} ({})", name.unwrap_or_default(), channel.id );
				} else {
					println!( "  {}", channel.id );
				}
			}

			println!();
		}

		return;
	} else if matches.opt_present( "L" ) {
		println!( "{}", bold.paint( "Subscribed topics: " ) );

		let mut topics = ytcli.topics( matches.opt_str( "t" ).unwrap_or_default() );
		topics.sort_by_cached_key( | t | { t.name.clone() } );

		for topic in topics {
			println!( "{} ({} channels)", topic.name, topic.channels.len() );
		}

		return;
	}

	let feed = YTFeed::from_topics( ytcli.topics( matches.opt_str( "t" ).unwrap_or_default() ) );
	if feed.videos.len() == 0 {
		println!( "There are no videos available." );
		return;
	}

	loop {
		let out = ytcli.skim( &feed );

		if out.len() > 0 {
			for v in &feed.videos {
				if v.text() == out[0].text() {
					Command::new( "mpv" )
						.arg( "--fullscreen" )
						.arg( v.url() )
						.spawn()
						.expect( "Failed to start mpv" )
						.wait()
						.expect( "Failed to wait for mpv" );
				}
			}
		} else {
			break
		};
	}

	if UEBERZUG_INIT.is_completed() {
		let utx = UEBERZUG_TX.lock().expect( "Failed to lock UEBERZUG_TX" );

		if utx.is_some() {
			utx
				.as_ref()
				.unwrap()
				.send( UeberzugAction::Exit )
				.expect( "Failed to close ueberzug" );
		}
	}
}
