use std::{fmt, str::FromStr};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pubkey([u8; 32]);

impl Pubkey {
    #[inline]
    pub fn to_bytes(self) -> [u8; 32] { self.0 }
}

impl fmt::Debug for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "Pubkey({self})") }
}

impl fmt::Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(&self.0).into_string())
    }
}

impl FromStr for Pubkey {
    type Err = bs58::decode::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut buf = [0_u8; 32];
        bs58::decode(&s).onto(&mut buf)?;
        Ok(Self(buf))
    }
}

impl TryFrom<String> for Pubkey {
    type Error = <Pubkey as FromStr>::Err;

    #[inline]
    fn try_from(value: String) -> Result<Self, Self::Error> { value.parse() }
}

impl TryFrom<&str> for Pubkey {
    type Error = <Pubkey as FromStr>::Err;

    #[inline]
    fn try_from(value: &str) -> Result<Pubkey, Self::Error> { value.parse() }
}
