use std;
use std::str;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use curl::easy::Easy;
use futures::Future;
use tokio_curl::Session;
use quick_xml::events::BytesStart;
use quick_xml::events::Event as XmlEvent;
use quick_xml::events::attributes::Attributes;
use quick_xml::reader::Reader as XmlReader;
use regex::Regex;

use errors::*;

lazy_static! {
    static ref HOST: Regex = Regex::new(r"^((?:https?://)?[^/]+)").unwrap();
}

pub trait FromXml: Sized {
    fn from_xml<B: std::io::BufRead>(reader: &mut XmlReader<B>, start: &BytesStart)
        -> Result<Self>;
}

fn parse_atom_link<B: std::io::BufRead>(
    reader: &mut XmlReader<B>,
    attributes: Attributes,
) -> Option<String> {
    let mut link_tmp = None;
    let mut is_alternate = true;
    for attribute in attributes {
        match attribute {
            Ok(attribute) => {
                match reader.decode(attribute.key).as_ref() {
                    "href" => {
                        match attribute.unescape_and_decode_value(reader) {
                            Ok(link) => link_tmp = Some(link),
                            Err(_) => continue,
                        }
                    }
                    "rel" => {
                        is_alternate = reader.decode(attribute.value).as_ref() == "alternate";
                    }
                    _ => (),
                }
            }
            Err(_) => continue,
        }
    }
    if is_alternate { link_tmp } else { None }
}

fn skip_element<B: std::io::BufRead>(reader: &mut XmlReader<B>) -> Result<()> {
    let mut buf = Vec::new();
    loop {
        match reader.read_event(&mut buf) {
            Ok(XmlEvent::Start(_)) => {
                skip_element(reader)?;
            }
            Ok(XmlEvent::End(_)) |
            Ok(XmlEvent::Eof) => break,
            Err(err) => return Err(err.into()),
            _ => (),
        }
        buf.clear();
    }
    Ok(())
}

impl FromXml for Option<String> {
    fn from_xml<B: std::io::BufRead>(
        reader: &mut XmlReader<B>,
        _start: &BytesStart,
    ) -> Result<Self> {
        let mut buf = Vec::new();
        let mut content: Option<String> = None;
        loop {
            match reader.read_event(&mut buf) {
                Ok(XmlEvent::Start(_)) => {
                    skip_element(reader)?;
                }
                Ok(XmlEvent::Text(ref e)) => {
                    let text = e.unescape_and_decode(reader)?;
                    content = Some(text);
                }
                Ok(XmlEvent::CData(ref e)) => {
                    let text = reader.decode(e).as_ref().to_owned();
                    content = Some(text);
                }
                Ok(XmlEvent::End(_)) |
                Ok(XmlEvent::Eof) => break,
                Err(err) => return Err(err.into()),
                _ => (),
            }
            buf.clear();
        }
        Ok(content)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RSS {
    pub title: String,
    pub link: String,
    pub items: Vec<Item>,
}

impl FromXml for RSS {
    fn from_xml<B: std::io::BufRead>(
        reader: &mut XmlReader<B>,
        _start: &BytesStart,
    ) -> Result<Self> {
        let mut buf = Vec::new();
        let mut rss = RSS::default();
        loop {
            match reader.read_event(&mut buf) {
                Ok(XmlEvent::Empty(ref e)) => {
                    if reader.decode(e.name()).as_ref() == "link" {
                        if let Some(link) = parse_atom_link(reader, e.attributes()) {
                            rss.link = link;
                        }
                    }
                }
                Ok(XmlEvent::Start(ref e)) => {
                    match reader.decode(e.name()).as_ref() {
                        "channel" => {
                            // RDF
                            let rdf = RSS::from_xml(reader, e)?;
                            rss.title = rdf.title;
                            rss.link = rdf.link;
                        }
                        "title" => {
                            if let Some(title) = Option::from_xml(reader, e)? {
                                rss.title = title;
                            }
                        }
                        "link" => {
                            if let Some(link) = Option::from_xml(reader, e)? {
                                // RSS
                                rss.link = link;
                            } else if let Some(link) = parse_atom_link(reader, e.attributes()) {
                                // ATOM
                                rss.link = link;
                            }
                        }
                        "item" | "entry" => {
                            rss.items.push(Item::from_xml(reader, e)?);
                        }
                        _ => skip_element(reader)?,
                    }
                }
                Ok(XmlEvent::End(_)) |
                Ok(XmlEvent::Eof) => break,
                Err(err) => return Err(err.into()),
                _ => (),
            }
            buf.clear();
        }
        Ok(rss)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    pub title: Option<String>,
    pub link: Option<String>,
    pub id: Option<String>,
}

impl FromXml for Item {
    fn from_xml<B: std::io::BufRead>(
        reader: &mut XmlReader<B>,
        _start: &BytesStart,
    ) -> Result<Self> {
        let mut buf = Vec::new();
        let mut item = Item::default();
        loop {
            match reader.read_event(&mut buf) {
                Ok(XmlEvent::Empty(ref e)) => {
                    if reader.decode(e.name()).as_ref() == "link" {
                        if let Some(link) = parse_atom_link(reader, e.attributes()) {
                            item.link = Some(link);
                        }
                    }
                }
                Ok(XmlEvent::Start(ref e)) => {
                    match reader.decode(e.name()).as_ref() {
                        "title" => {
                            item.title = Option::from_xml(reader, e)?;
                        }
                        "link" => {
                            if let Some(link) = Option::from_xml(reader, e)? {
                                // RSS
                                item.link = Some(link);
                            } else if let Some(link) = parse_atom_link(reader, e.attributes()) {
                                // ATOM
                                item.link = Some(link);
                            }
                        }
                        "id" | "guid" => {
                            item.id = Option::from_xml(reader, e)?;
                        }
                        _ => skip_element(reader)?,
                    }
                }
                Ok(XmlEvent::End(_)) |
                Ok(XmlEvent::Eof) => break,
                Err(err) => return Err(err.into()),
                _ => (),
            }
            buf.clear();
        }
        Ok(item)
    }
}

pub fn parse<B: std::io::BufRead>(reader: B) -> Result<RSS> {
    let mut reader = XmlReader::from_reader(reader);
    reader.trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                match reader.decode(e.name()).as_ref() {
                    "rss" => continue,
                    "channel" | "feed" | "rdf:RDF" => {
                        return RSS::from_xml(&mut reader, e);
                    }
                    _ => skip_element(&mut reader)?,
                }
            }
            Ok(XmlEvent::Eof) => return Err(ErrorKind::EOF.into()),
            Err(err) => return Err(err.into()),
            _ => (),
        }
        buf.clear();
    }
}

fn set_url_relative_to_absolute(link: &mut String, host: &str) {
    match link.as_str() {
        _ if link.starts_with("//") => {
            let mut s = String::from("http:");
            s.push_str(link);
            *link = s;
        }
        _ if link.starts_with('/') => {
            let mut s = String::from(host);
            s.push_str(link);
            *link = s;
        }
        _ => (),
    }
}

fn fix_relative_url(mut rss: RSS, rss_link: &str) -> RSS {
    let rss_host = HOST.captures(rss_link).map_or(rss_link, |r| {
        r.get(0).unwrap().as_str()
    });
    match rss.link.as_str() {
        "" | "/" => rss.link = rss_host.to_owned(),
        _ => set_url_relative_to_absolute(&mut rss.link, rss_host),
    }
    for item in &mut rss.items {
        if let Some(link) = item.link.as_mut() {
            set_url_relative_to_absolute(link, rss_host);
        }
    }

    rss
}

pub fn fetch_feed<'a>(
    session: &Session,
    link: String,
) -> impl Future<Item = RSS, Error = Error> + 'a {
    let mut req = Easy::new();
    let buf = Arc::new(Mutex::new(Vec::new()));
    {
        let buf = buf.clone();
        req.get(true).unwrap();
        req.url(&link).unwrap();
        req.accept_encoding("").unwrap(); // accept all encoding
        req.useragent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION"),
            " (",
            env!("CARGO_PKG_HOMEPAGE"),
            ")"
        )).unwrap();
        req.follow_location(true).unwrap();
        req.timeout(Duration::from_secs(10)).unwrap();
        req.write_function(move |data| {
            buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }).unwrap();
    }
    session.perform(req).map_err(|e| e.into()).and_then(
        move |mut resp| {
            let response_code = resp.response_code().unwrap();
            if response_code != 200 {
                return Err(ErrorKind::Http(response_code).into());
            }
            let buf = buf.lock().unwrap();
            let rss = parse(buf.as_slice())?;
            Ok(fix_relative_url(rss, &link))
        },
    )
}

#[test]
fn test_host_regex() {
    assert!(HOST.captures("").is_none());
    assert!(HOST.captures("/path").is_none());
    assert_eq!(
        &HOST.captures("example.com/path").unwrap()[0],
        "example.com"
    );
    assert_eq!(
        &HOST.captures("http://example.com/path").unwrap()[0],
        "http://example.com"
    );
    assert_eq!(
        &HOST.captures("https://example.com/path").unwrap()[0],
        "https://example.com"
    );
}
