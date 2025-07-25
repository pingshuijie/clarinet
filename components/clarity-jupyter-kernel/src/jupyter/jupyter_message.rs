use super::connection::{Connection, HmacSha256};
use chrono::Utc;
use failure::Error;
use hex;
use json::{self, JsonValue};
use std::{self, fmt};
use uuid::Uuid;

struct RawMessage {
    zmq_identities: Vec<Vec<u8>>,
    jparts: Vec<Vec<u8>>,
}

impl RawMessage {
    pub fn read(connection: &mut Connection) -> Result<RawMessage, Error> {
        let mut multipart = connection.socket.recv_multipart(0)?;
        let delimiter_index = multipart
            .iter()
            .position(|part| &part[..] == DELIMITER)
            .ok_or_else(|| format_err!("Missing delimiter"))?;
        let jparts: Vec<_> = multipart.drain(delimiter_index + 2..).collect();
        let hmac = multipart.pop().unwrap();
        // Remove delimiter, so that what's left is just the identities.
        multipart.pop();
        let zmq_identities = multipart;

        let raw_message = RawMessage {
            zmq_identities,
            jparts,
        };

        if let Some(mac_template) = &connection.mac {
            let mut mac = mac_template.clone();
            raw_message.digest(&mut mac);
            use hmac::Mac;
            if let Err(error) = mac.verify(&hex::decode(&hmac)?) {
                bail!("{}", error);
            }
        }

        Ok(raw_message)
    }

    fn send(self, connection: &mut Connection) -> Result<(), Error> {
        use hmac::Mac;
        let hmac = if let Some(mac_template) = &connection.mac {
            let mut mac = mac_template.clone();
            self.digest(&mut mac);
            hex::encode(mac.result().code().as_slice())
        } else {
            String::new()
        };
        let mut parts: Vec<&[u8]> = Vec::new();
        for part in &self.zmq_identities {
            parts.push(part);
        }
        parts.push(DELIMITER);
        parts.push(hmac.as_bytes());
        for part in &self.jparts {
            parts.push(part);
        }
        connection.socket.send_multipart(&parts, 0)?;
        Ok(())
    }

    fn digest(&self, mac: &mut HmacSha256) {
        use hmac::Mac;
        for part in &self.jparts {
            mac.input(&part);
        }
    }
}

#[derive(Clone)]
pub struct JupyterMessage {
    zmq_identities: Vec<Vec<u8>>,
    header: JsonValue,
    parent_header: JsonValue,
    metadata: JsonValue,
    content: JsonValue,
}

const DELIMITER: &[u8] = b"<IDS|MSG>";

impl JupyterMessage {
    pub fn read(connection: &mut Connection) -> Result<JupyterMessage, Error> {
        let raw_message = RawMessage::read(connection)?;

        fn message_to_json(message: &[u8]) -> Result<JsonValue, Error> {
            Ok(json::parse(std::str::from_utf8(message)?)?)
        }

        if raw_message.jparts.len() < 4 {
            bail!("Insufficient message parts {}", raw_message.jparts.len());
        }

        Ok(JupyterMessage {
            zmq_identities: raw_message.zmq_identities,
            header: message_to_json(&raw_message.jparts[0])?,
            parent_header: message_to_json(&raw_message.jparts[1])?,
            metadata: message_to_json(&raw_message.jparts[2])?,
            content: message_to_json(&raw_message.jparts[3])?,
        })
    }

    pub fn message_type(&self) -> &str {
        self.header["msg_type"].as_str().unwrap_or("")
    }

    pub fn code(&self) -> &str {
        self.content["code"].as_str().unwrap_or("")
    }

    // Creates a new child message of this message. ZMQ identities are not transferred.
    pub fn new_message(&self, msg_type: &str) -> JupyterMessage {
        let mut header = self.header.clone();
        header["msg_type"] = JsonValue::String(msg_type.to_owned());
        header["username"] = JsonValue::String("kernel".to_owned());
        header["msg_id"] = JsonValue::String(Uuid::new_v4().to_string());
        header["date"] = JsonValue::String(Utc::now().to_rfc3339());

        JupyterMessage {
            zmq_identities: Vec::new(),
            header,
            parent_header: self.header.clone(),
            metadata: JsonValue::new_object(),
            content: JsonValue::new_object(),
        }
    }

    pub fn new_reply(&self) -> JupyterMessage {
        let mut reply = self.new_message(&self.message_type().replace("_request", "_reply"));
        reply.zmq_identities = self.zmq_identities.clone();
        reply
    }

    pub fn get_content(&self) -> &JsonValue {
        &self.content
    }

    pub fn with_content(mut self, content: JsonValue) -> JupyterMessage {
        self.content = content;
        self
    }

    pub fn send(&self, connection: &mut Connection) -> Result<(), Error> {
        let raw_message = RawMessage {
            zmq_identities: self.zmq_identities.clone(),
            jparts: vec![
                self.header.dump().as_bytes().to_vec(),
                self.parent_header.dump().as_bytes().to_vec(),
                self.metadata.dump().as_bytes().to_vec(),
                self.content.dump().as_bytes().to_vec(),
            ],
        };
        raw_message.send(connection)
    }
}

impl fmt::Debug for JupyterMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nHEADER {}", self.header.pretty(2))?;
        writeln!(f, "PARENT_HEADER {}", self.parent_header.pretty(2))?;
        writeln!(f, "METADATA {}", self.metadata.pretty(2))?;
        writeln!(f, "CONTENT {}\n", self.content.pretty(2))?;
        Ok(())
    }
}
