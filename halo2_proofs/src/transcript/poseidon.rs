use poseidon::Poseidon;

use crate::arithmetic::{Coordinates, CurveAffine, Field};
use crate::transcript::{EncodedChallenge, Transcript, TranscriptRead, TranscriptWrite};
use group::ff::PrimeField;

use std::io::{self, Read, Write};
use std::marker::PhantomData;

/// x and y coordinates of a point are in base field. With
/// strategies implements PointRepresentation base field elements are encoded as
/// scalar field elements.
pub trait PointRepresentation<C: CurveAffine> {
    /// Given point returns elements that should be written to the state
    fn encode(point: C) -> io::Result<Vec<C::Scalar>>;
    /// Returns x and y coordinates. Panics if point is at infinity
    fn xy(point: C) -> io::Result<(C::Base, C::Base)> {
        let coords: Coordinates<C> = Option::from(point.coordinates()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "cannot write points at infinity to the transcript",
            )
        })?;
        Ok((*coords.x(), *coords.y()))
    }
}

/// Decomposes `x` coordinate which is a base field element into smaller limbs
/// that should fit into the scalar field size. In order to avoid contribution
/// only sign of the `y` coordinate which is one or zero is appended after `x`.
#[derive(Debug)]
pub struct LimbRepresentation<C: CurveAffine, const NUMBER_OF_LIMBS: usize, const BITLEN: usize> {
    _marker: PhantomData<C>,
}

impl<C: CurveAffine, const NUMBER_OF_LIMBS: usize, const BITLEN: usize> PointRepresentation<C>
    for LimbRepresentation<C, NUMBER_OF_LIMBS, BITLEN>
{
    fn encode(point: C) -> io::Result<Vec<C::Scalar>> {
        assert!(bool::from(point.is_on_curve()));
        assert!(!bool::from(point.is_identity()));
        let (x, y) = Self::xy(point)?;
        let mut encoded: Vec<C::Scalar> = decompose(x, NUMBER_OF_LIMBS, BITLEN);
        encoded.push(if sign(y) {
            C::Scalar::one()
        } else {
            C::Scalar::zero()
        });
        Ok(encoded)
    }
}

/// Native representation approach assumes there is no such `P0` and `P1` points
/// that satisfies `(x_0 == x_1) mod n` and `(y_0 == y_1) mod n` where n is
/// scalar field moduli. This approach might be completely wrong so just don't
/// use it.
#[derive(Debug)]
pub struct NativeRepresentation<C: CurveAffine> {
    _marker: PhantomData<C>,
}

impl<C: CurveAffine> PointRepresentation<C> for NativeRepresentation<C> {
    fn encode(point: C) -> io::Result<Vec<C::Scalar>> {
        let (x, y) = Self::xy(point)?;
        Ok(vec![native(x), native(y)])
    }
}

/// Transcript reader with Poseidon
#[derive(Debug, Clone)]
pub struct PoseidonRead<
    R: Read,
    C: CurveAffine,
    E: EncodedChallenge<C>,
    Z: PointRepresentation<C>,
    const T: usize,
    const RATE: usize,
> {
    state: Poseidon<C::Scalar, T, RATE>,
    reader: R,
    _marker: PhantomData<(E, Z)>,
}

impl<
        R: Read,
        C: CurveAffine,
        E: EncodedChallenge<C>,
        Z: PointRepresentation<C>,
        const T: usize,
        const RATE: usize,
    > PoseidonRead<R, C, E, Z, T, RATE>
{
    /// Initialize a transcript given an input buffer.
    pub fn init(reader: R, r_f: usize, r_p: usize) -> Self {
        PoseidonRead {
            state: Poseidon::new(r_f, r_p),
            reader,
            _marker: PhantomData,
        }
    }
}

impl<R: Read, C: CurveAffine, Z: PointRepresentation<C>, const T: usize, const RATE: usize>
    TranscriptRead<C, Challenge<C>> for PoseidonRead<R, C, Challenge<C>, Z, T, RATE>
{
    fn read_point(&mut self) -> io::Result<C> {
        let mut compressed = C::Repr::default();
        self.reader.read_exact(compressed.as_mut())?;
        let point: C = Option::from(C::from_bytes(&compressed)).ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "invalid point encoding in proof")
        })?;
        self.common_point(point)?;

        Ok(point)
    }

    fn read_scalar(&mut self) -> io::Result<C::Scalar> {
        let mut data = <C::Scalar as PrimeField>::Repr::default();
        self.reader.read_exact(data.as_mut())?;
        let scalar: C::Scalar = Option::from(C::Scalar::from_repr(data)).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "invalid field element encoding in proof",
            )
        })?;
        self.common_scalar(scalar)?;

        Ok(scalar)
    }
}

impl<R: Read, C: CurveAffine, Z: PointRepresentation<C>, const T: usize, const RATE: usize>
    Transcript<C, Challenge<C>> for PoseidonRead<R, C, Challenge<C>, Z, T, RATE>
{
    fn squeeze_challenge(&mut self) -> Challenge<C> {
        self.state.update(&[/* PREFIX_CHALLENGE */]);
        // TODO/FIX: should we clone state at this point or are we ok with squeezing
        // advances the state?
        Challenge::<C>::new(&self.state.squeeze())
    }

    fn common_point(&mut self, point: C) -> io::Result<()> {
        self.state.update(&[/* PREFIX_POINT */]);
        self.state.update(&Z::encode(point)?[..]);

        Ok(())
    }

    fn common_scalar(&mut self, scalar: C::Scalar) -> io::Result<()> {
        self.state.update(&[/* PREFIX_SCALAR */]);
        self.state.update(&[scalar]);

        Ok(())
    }
}

/// Poseidon transcript writer.
#[derive(Debug, Clone)]
pub struct PoseidonWrite<
    W: Write,
    C: CurveAffine,
    E: EncodedChallenge<C>,
    Z: PointRepresentation<C>,
    const T: usize,
    const RATE: usize,
> {
    state: Poseidon<C::Scalar, T, RATE>,
    writer: W,
    _marker: PhantomData<(E, Z)>,
}

impl<
        W: Write,
        C: CurveAffine,
        E: EncodedChallenge<C>,
        Z: PointRepresentation<C>,
        const T: usize,
        const RATE: usize,
    > PoseidonWrite<W, C, E, Z, T, RATE>
{
    /// Initialize a transcript given an output buffer and round parameters
    pub fn init(writer: W, r_f: usize, r_p: usize) -> Self {
        PoseidonWrite {
            state: Poseidon::new(r_f, r_p),
            writer,
            _marker: PhantomData,
        }
    }

    /// Conclude the interaction and return the output buffer (writer).
    pub fn finalize(self) -> W {
        self.writer
    }
}

impl<W: Write, C: CurveAffine, Z: PointRepresentation<C>, const T: usize, const RATE: usize>
    TranscriptWrite<C, Challenge<C>> for PoseidonWrite<W, C, Challenge<C>, Z, T, RATE>
{
    fn write_point(&mut self, point: C) -> io::Result<()> {
        self.common_point(point)?;
        let compressed = point.to_bytes();
        self.writer.write_all(compressed.as_ref())
    }
    fn write_scalar(&mut self, scalar: C::Scalar) -> io::Result<()> {
        self.common_scalar(scalar)?;
        let data = scalar.to_repr();
        self.writer.write_all(data.as_ref())
    }
}

impl<W: Write, C: CurveAffine, Z: PointRepresentation<C>, const T: usize, const RATE: usize>
    Transcript<C, Challenge<C>> for PoseidonWrite<W, C, Challenge<C>, Z, T, RATE>
{
    fn squeeze_challenge(&mut self) -> Challenge<C> {
        self.state.update(&[/* PREFIX_CHALLENGE */]);
        // TODO/FIX: should we clone state at this point or are we ok with squeezing
        // advances the state?
        Challenge::<C>::new(&self.state.squeeze())
    }

    fn common_point(&mut self, point: C) -> io::Result<()> {
        self.state.update(&[/* PREFIX_POINT */]);
        self.state.update(&Z::encode(point)?[..]);

        Ok(())
    }

    fn common_scalar(&mut self, scalar: C::Scalar) -> io::Result<()> {
        self.state.update(&[/* PREFIX_SCALAR */]);
        self.state.update(&[scalar]);

        Ok(())
    }
}

use num_bigint::BigUint;
use num_traits::Num;
use pairing::arithmetic::{BaseExt, FieldExt};
use std::ops::Shl;

use super::Challenge;

pub(crate) fn decompose<Base: BaseExt, Scalar: FieldExt>(
    e: Base,
    number_of_limbs: usize,
    bit_len: usize,
) -> Vec<Scalar> {
    decompose_big(fe_to_big(e), number_of_limbs, bit_len)
}

fn native<Base: BaseExt, Scalar: FieldExt>(e: Base) -> Scalar {
    big_to_fe(fe_to_big(e) % modulus::<Scalar>())
}

fn modulus<F: FieldExt>() -> BigUint {
    BigUint::from_str_radix(&F::MODULUS[2..], 16).unwrap()
}

fn decompose_big<F: FieldExt>(e: BigUint, number_of_limbs: usize, bit_len: usize) -> Vec<F> {
    let mut e = e;
    let mask = BigUint::from(1usize).shl(bit_len) - 1usize;
    let limbs: Vec<F> = (0..number_of_limbs)
        .map(|_| {
            let limb = mask.clone() & e.clone();
            e = e.clone() >> bit_len;
            big_to_fe(limb)
        })
        .collect();

    limbs
}

fn big_to_fe<F: FieldExt>(e: BigUint) -> F {
    let modulus = modulus::<F>();
    let e = e % modulus;
    let mut bytes = e.to_bytes_le();
    bytes.resize(32, 0);
    let mut bytes = &bytes[..];
    F::read(&mut bytes).unwrap()
}

fn fe_to_big<F: BaseExt>(fe: F) -> BigUint {
    let mut bytes: Vec<u8> = Vec::new();
    fe.write(&mut bytes).unwrap();
    BigUint::from_bytes_le(&bytes[..])
}

fn sign<F: BaseExt>(fe: F) -> bool {
    let mut bytes: Vec<u8> = Vec::new();
    fe.write(&mut bytes).unwrap();
    (bytes[0] & 1) == 0
}

#[test]
fn test_transcript() {
    use pairing::bn256::{Fr, G1Affine};
    use rand::thread_rng;
    let mut rng = thread_rng();

    const R_F: usize = 8;
    const R_P: usize = 57;

    macro_rules! init_writer {
        () => {
            PoseidonWrite::<_, G1Affine, Challenge<_>, LimbRepresentation<_, 4, 68>, 3, 2>::init(
                vec![],
                R_F,
                R_P,
            )
        };
    }

    macro_rules! init_reader {
        ($proof:expr) => {
            PoseidonRead::<_, G1Affine, Challenge<_>, LimbRepresentation<_, 4, 68>, 3, 2>::init(
                &$proof[..],
                R_F,
                R_P,
            )
        };
    }

    let mut writer = init_writer!();
    let s_0 = writer.squeeze_challenge();
    let proof = writer.finalize();

    let mut reader = init_reader!(proof);
    let s_1 = reader.squeeze_challenge();
    assert_eq!(s_0.get_scalar(), s_1.get_scalar());

    let p0 = G1Affine::random(&mut rng);
    let p1 = G1Affine::random(&mut rng);
    let p2 = G1Affine::random(&mut rng);
    let p3 = G1Affine::random(&mut rng);
    let e0 = Fr::random(&mut rng);
    let e1 = Fr::random(&mut rng);
    let e2 = Fr::random(&mut rng);
    let e3 = Fr::random(&mut rng);

    let mut writer = init_writer!();
    writer.write_point(p0).unwrap();
    writer.write_point(p1).unwrap();
    writer.write_scalar(e0).unwrap();
    writer.write_scalar(e1).unwrap();
    writer.write_scalar(e2).unwrap();
    writer.write_scalar(e3).unwrap();
    writer.write_point(p2).unwrap();
    writer.write_point(p3).unwrap();
    let s_0 = writer.squeeze_challenge();
    let proof = writer.finalize();

    let mut reader = init_reader!(proof);
    assert_eq!(reader.read_point().unwrap(), p0);
    assert_eq!(reader.read_point().unwrap(), p1);
    assert_eq!(reader.read_scalar().unwrap(), e0);
    assert_eq!(reader.read_scalar().unwrap(), e1);
    assert_eq!(reader.read_scalar().unwrap(), e2);
    assert_eq!(reader.read_scalar().unwrap(), e3);
    assert_eq!(reader.read_point().unwrap(), p2);
    assert_eq!(reader.read_point().unwrap(), p3);
    let s_1 = reader.squeeze_challenge();
    assert_eq!(s_0.get_scalar(), s_1.get_scalar());
}