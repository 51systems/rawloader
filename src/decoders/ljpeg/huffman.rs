/*
* Huffman table generation:
* HuffTable::huff_decode()
* HuffTable::initialize()
* and used data structures are originally grabbed from the IJG software,
* and adapted by Hubert Figuiere.
*
* Copyright (C) 1991, 1992, Thomas G. Lane.
* Part of the Independent JPEG Group's software.
* See the file Copyright for more details.
*
* Copyright (c) 1993 Brian C. Smith, The Regents of the University
* of California
* All rights reserved.
*
* Copyright (c) 1994 Kongji Huang and Brian C. Smith.
* Cornell University
* All rights reserved.
*
* Permission to use, copy, modify, and distribute this software and its
* documentation for any purpose, without fee, and without written agreement is
* hereby granted, provided that the above copyright notice and the following
* two paragraphs appear in all copies of this software.
*
* IN NO EVENT SHALL CORNELL UNIVERSITY BE LIABLE TO ANY PARTY FOR
* DIRECT, INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES ARISING OUT
* OF THE USE OF THIS SOFTWARE AND ITS DOCUMENTATION, EVEN IF CORNELL
* UNIVERSITY HAS BEEN ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*
* CORNELL UNIVERSITY SPECIFICALLY DISCLAIMS ANY WARRANTIES,
* INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY
* AND FITNESS FOR A PARTICULAR PURPOSE.  THE SOFTWARE PROVIDED HEREUNDER IS
* ON AN "AS IS" BASIS, AND CORNELL UNIVERSITY HAS NO OBLIGATION TO
* PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS.
*/

use std::fmt;
use decoders::basics::*;

const BIG_TABLE_BITS: u32 = 13;

const HUFF_BITMASK: [u32;32] = [0xffffffff, 0x7fffffff,
                                0x3fffffff, 0x1fffffff,
                                0x0fffffff, 0x07ffffff,
                                0x03ffffff, 0x01ffffff,
                                0x00ffffff, 0x007fffff,
                                0x003fffff, 0x001fffff,
                                0x000fffff, 0x0007ffff,
                                0x0003ffff, 0x0001ffff,
                                0x0000ffff, 0x00007fff,
                                0x00003fff, 0x00001fff,
                                0x00000fff, 0x000007ff,
                                0x000003ff, 0x000001ff,
                                0x000000ff, 0x0000007f,
                                0x0000003f, 0x0000001f,
                                0x0000000f, 0x00000007,
                                0x00000003, 0x00000001];

pub struct HuffTable {
  // These two fields directly represent the contents of a JPEG DHT marker
  pub bits: [u32;17],
  pub huffval: [u32;256],

  // The remaining fields are computed from the above to allow more
  // efficient coding and decoding and thus private
  mincode: [u16;17],
  maxcode: [i32;18],
  valptr: [i16;17],
  numbits: [u32;256],
  bigtable: Vec<i32>,
  precision: usize,
  pub use_bigtable: bool,
  pub dng_bug: bool,
  pub initialized: bool,
}

impl HuffTable {
  pub fn empty(precision: usize) -> HuffTable {
    HuffTable {
      bits: [0;17],
      huffval: [0;256],
      mincode: [0;17],
      maxcode: [0;18],
      valptr: [0;17],
      numbits: [0;256],
      bigtable: Vec::new(),
      precision: precision,
      use_bigtable: true,
      dng_bug: false,
      initialized: false,
    }
  }

  pub fn new(bits: [u32;17], huffval: [u32;256], precision: usize, dng_bug: bool) -> Result<HuffTable,String> {
    let mut tbl = HuffTable {
      bits: bits,
      huffval: huffval,
      mincode: [0;17],
      maxcode: [0;18],
      valptr: [0;17],
      numbits: [0;256],
      bigtable: Vec::new(),
      precision: precision,
      use_bigtable: true,
      dng_bug: dng_bug,
      initialized: false,
    };
    // Always use big table, haven't found a situation where it doesn't help
    try!(tbl.initialize(true));
    Ok(tbl)
  }

  pub fn initialize(&mut self, use_bigtable: bool) -> Result<(), String> {
    // Figure C.1: make table of Huffman code length for each symbol
    // Note that this is in code-length order.
    let mut p = 0;
    let mut huffsize: [u8;257] = [0;257];
    for l in 1..17 {
      for _ in 1..((self.bits[l] as usize)+1) {
        huffsize[p] = l as u8;
        p += 1;
        if p > 256 {
          return Err("ljpeg: Code length too long. Corrupt data.".to_string())
        }
      }
    }
    huffsize[p] = 0;
    let lastp = p;

    // Figure C.2: generate the codes themselves
    // Note that this is in code-length order.
    let mut code: u16 = 0;
    let mut si: u32 = huffsize[0] as u32;
    let mut huffcode: [u16;257] = [0;257];
    p = 0;
    while huffsize[p] > 0 {
      while (huffsize[p] as u32) == si {
        huffcode[p] = code;
        p += 1;
        code += 1;
      }
      code <<= 1;
      si += 1;
      if p > 256 {
        return Err("ljpeg: Code length too long. Corrupt data.".to_string())
      }
    }


    //Figure F.15: generate decoding tables
    self.mincode[0] = 0;
    self.maxcode[0] = 0;
    p = 0;
    for l in 1..17 {
      if self.bits[l] > 0 {
        self.valptr[l] = p as i16;
        self.mincode[l] = huffcode[p];
        p += self.bits[l] as usize;
        self.maxcode[l] = huffcode[p - 1] as i32;
      } else {
        self.valptr[l] = 0xff;   // This check must be present to avoid crash on junk
        self.maxcode[l] = -1;
      }
      if p > 256 {
        return Err("ljpeg: Code length too long. Corrupt data.".to_string())
      }
    }

    // We put in this value to ensure HuffDecode terminates
    self.maxcode[17] = 0xFFFFF;

    // Build the numbits, value lookup tables.
    // These table allow us to gather 8 bits from the bits stream,
    // and immediately lookup the size and value of the huffman codes.
    // If size is zero, it means that more than 8 bits are in the huffman
    // code (this happens about 3-4% of the time).
    for p in 0..lastp {
      let size = huffsize[p];
      if size <= 8 {
        let value: i32 = self.huffval[p] as i32;
        let code = huffcode[p];
        let ll: i32 = (code << (8 - size)) as i32;
        let ul: i32 = if size < 8 {
          ll | (HUFF_BITMASK[(24+size) as usize]) as i32
        } else {
          ll
        };
        if ul > 256 || ll > ul {
          return Err("ljpeg: Code length too long. Corrupt data.".to_string())
        }
        for i in ll..(ul+1) {
          self.numbits[i as usize] = (size as u32) | ((value as u32) << 4);
        }
      }
    }

    if use_bigtable {
      self.initialize_bigtable();
    }
    self.initialized = true;
    self.use_bigtable = use_bigtable;
    Ok(())
  }

  fn initialize_bigtable(&mut self) {
    let size: usize = 1 << BIG_TABLE_BITS;

    self.bigtable = vec![0 as i32;size];
    let mut rv: i32;
    for i in 0..size {
      let input = (i << (16-BIG_TABLE_BITS)) as u16;
      let mut code: i32 = (input >> 8) as i32;
      let val = self.numbits[code as usize];
      let mut l: u32 = val & 15;
      if l > 0 {
        rv = (val >> 4) as i32;
      } else {
        l = 8;
        while code > self.maxcode[l as usize] && l < 16 {
          let temp: i32 = (input >> (15 - l) & 1) as i32;
          code = (code << 1) | temp;
          l += 1;
        }

        //With garbage input we may reach the sentinel value l = 17.
        if l > self.precision as u32 || self.valptr[l as usize] == 0xff {
          self.bigtable[i] = 0xff;
          continue
        } else {
          rv = self.huffval[
            self.valptr[l as usize] as usize +
            (code - (self.mincode[l as usize] as i32)) as usize
          ] as i32;
        }
      }

      if rv == 16 {
        self.bigtable[i] = if self.dng_bug {
          (-(32768 << 8)) | (16 + l as i32)
        } else {
          (-(32768 << 8)) | l as i32
        };
        continue
      }

      if rv + l as i32 > BIG_TABLE_BITS as i32 {
        self.bigtable[i] = 0xff;
        continue
      }

      if rv != 0 {
        let mut x = (input >> (16 - (l as i32) - rv) & ((1 << rv) - 1)) as i32;
        if (x & (1 << (rv - 1))) == 0 {
          x -= (1 << rv) - 1;
        }
        self.bigtable[i] = (x << 8) | ((l as i32) + rv);
      } else {
        self.bigtable[i] = l as i32;
      }
    }
    self.use_bigtable = true;
  }

  // Taken from Figure F.16: extract next coded symbol from input stream
  pub fn huff_decode(&self, pump: &mut BitPump) -> Result<i32,String> {
    //First attempt to do complete decode, by using the first 14 bits
    if self.use_bigtable {
      let code = pump.peek_bits(BIG_TABLE_BITS) as usize;
      let val: i32 = self.bigtable[code];
      if val & 0xff != 0xff {
        pump.consume_bits((val & 0xff) as u32);
        return Ok(val >> 8)
      }
    }

    let len = try!(self.huff_len(pump));
    let diff = self.huff_diff(pump, len);
    Ok(diff)
  }

  pub fn huff_len(&self, pump: &mut BitPump) -> Result<u32,String> {
    let mut code = pump.peek_bits(8) as usize;
    let val = self.numbits[code as usize] as u32;
    let len = val & 15;
    if len != 0 {
      pump.consume_bits(len);
      return Ok(val >> 4)
    }
    pump.consume_bits(8);
    let mut l: usize = 8;
    while code as i32 > self.maxcode[l] {
      let temp = pump.get_bits(1) as usize;
      code = (code << 1) | temp;
      l += 1;
    }

    // With garbage input we may reach the sentinel value l = 17.
    if l > self.precision || self.valptr[l] == 0xff {
      return Err(format!("ljpeg: bad Huffman code: {}", l).to_string())
    } else {
      return Ok(self.huffval[
        self.valptr[l] as usize +
        (code - (self.mincode[l] as usize)) as usize
      ]);
    }
  }

  pub fn huff_diff(&self, pump: &mut BitPump, len: u32) -> i32 {
    match len {
      0 => 0,
      16 => {
        if self.dng_bug {
          pump.get_bits(16); // consume can fail because we haven't peeked yet
        }
        -32768
      },
      len => {
        // decode the difference and extend sign bit
        let mut diff = pump.get_bits(len) as i32;
        if (diff & (1 << (len - 1))) == 0 {
          diff -= (1 << len) - 1;
        }
        diff
      },
    }
  }
}

impl fmt::Debug for HuffTable {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    if self.initialized {
      write!(f, "HuffTable {{ bits: {:?} huffval: {:?} }}", self.bits, &self.huffval[..])
    } else {
      write!(f, "HuffTable {{ uninitialized }}")
    }
  }
}
