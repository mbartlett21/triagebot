use crate::error::Error;
use crate::token::{Token, Tokenizer};

#[derive(PartialEq, Eq, Debug)]
pub struct SecondCommand;

impl SecondCommand {
    pub fn parse<'a>(input: &mut Tokenizer<'a>) -> Result<Option<Self>, Error<'a>> {
        if let Some(Token::Word("second" | "seconded")) = input.peek_token()? {
            Ok(Some(Self))
        } else {
            Ok(None)
        }
    }
}
