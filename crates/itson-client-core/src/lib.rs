//! Toolkit-independent client state. The GUI and the headless CLI both drive
//! this; nothing here touches a socket.

pub mod cache;
pub mod connection;
