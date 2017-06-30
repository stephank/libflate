use std::io;
use std::io::Read;
use std::cmp;
use std::ptr;
use byteorder::ReadBytesExt;
use byteorder::LittleEndian;

use bit;
use lz77;
use util;
use deflate::symbol;

#[derive(Debug)]
pub struct Decoder<R> {
    bit_reader: bit::BitReader<R>,
    buffer: Vec<u8>,
    offset: usize,
    eos: bool,
}
impl<R> Decoder<R>
where
    R: Read,
{
    pub fn new(inner: R) -> Self {
        Decoder {
            bit_reader: bit::BitReader::new(inner),
            buffer: Vec::new(),
            offset: 0,
            eos: false,
        }
    }
    fn read_non_compressed_block(&mut self) -> io::Result<()> {
        self.bit_reader.reset();
        let len = self.bit_reader.as_inner_mut().read_u16::<LittleEndian>()?;
        let nlen = self.bit_reader.as_inner_mut().read_u16::<LittleEndian>()?;
        if !len != nlen {
            Err(invalid_data_error!(
                "LEN={} is not the one's complement of NLEN={}",
                len,
                nlen
            ))
        } else {
            let old_len = self.buffer.len();
            self.buffer.reserve(len as usize);
            unsafe { self.buffer.set_len(old_len + len as usize) };
            self.bit_reader.as_inner_mut().read_exact(
                &mut self.buffer[old_len..],
            )?;
            Ok(())
        }
    }
    fn read_compressed_block<H>(&mut self, huffman: H) -> io::Result<()>
    where
        H: symbol::HuffmanCodec,
    {
        let symbol_decoder = huffman.load(&mut self.bit_reader)?;
        loop {
            let s = symbol_decoder.decode_unchecked(&mut self.bit_reader);
            self.bit_reader.check_last_error()?;
            match s {
                symbol::Symbol::Literal(b) => {
                    self.buffer.push(b);
                }
                symbol::Symbol::Share { length, distance } => {
                    if self.buffer.len() < distance as usize {
                        let msg = format!(
                            "Too long backword reference: buffer.len={}, distance={}",
                            self.buffer.len(),
                            distance
                        );
                        return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                    }
                    let old_len = self.buffer.len();
                    self.buffer.reserve(length as usize);
                    unsafe {
                        self.buffer.set_len(old_len + length as usize);
                        let start = old_len - distance as usize;
                        let ptr = self.buffer.as_mut_ptr();
                        util::ptr_copy(
                            ptr.offset(start as isize),
                            ptr.offset(old_len as isize),
                            length as usize,
                            length > distance,
                        );
                    }
                }
                symbol::Symbol::EndOfBlock => {
                    break;
                }
            }
        }
        Ok(())
    }
    fn truncate_old_buffer(&mut self) {
        if self.buffer.len() > lz77::MAX_DISTANCE as usize * 4 {
            let new_len = lz77::MAX_DISTANCE as usize;
            unsafe {
                let ptr = self.buffer.as_mut_ptr();
                let src = ptr.offset((self.buffer.len() - new_len) as isize);
                ptr::copy_nonoverlapping(src, ptr, new_len);
            }
            self.buffer.truncate(new_len);
            self.offset = new_len;
        }
    }
}
impl<R> Read for Decoder<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset < self.buffer.len() {
            let copy_size = cmp::min(buf.len(), self.buffer.len() - self.offset);
            buf[..copy_size].copy_from_slice(&self.buffer[self.offset..][..copy_size]);
            self.offset += copy_size;
            Ok(copy_size)
        } else if self.eos {
            Ok(0)
        } else {
            let bfinal = self.bit_reader.read_bit()?;
            let btype = self.bit_reader.read_bits(2)?;
            self.eos = bfinal;
            self.truncate_old_buffer();
            match btype {
                0b00 => {
                    self.read_non_compressed_block()?;
                    self.read(buf)
                }
                0b01 => {
                    self.read_compressed_block(symbol::FixedHuffmanCodec)?;
                    self.read(buf)
                }
                0b10 => {
                    self.read_compressed_block(symbol::DynamicHuffmanCodec)?;
                    self.read(buf)
                }
                0b11 => Err(invalid_data_error!(
                    "btype 0x11 of DEFLATE is reserved(error) value"
                )),
                _ => unreachable!(),
            }
        }
    }
}