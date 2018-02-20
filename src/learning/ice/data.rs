//! Contains stuff related to ICE's projected data representation.

use common::* ;
use common::data::{ DataCore, Sample, HSample, HSamples } ;

use super::quals::{ Qual } ;


/// Projected data to classify.
#[derive(Clone)]
pub struct CData {
  /// Positive samples.
  pos: HSamples,
  /// Negative samples.
  neg: HSamples,
  /// Unclassified samples.
  unc: HSamples,
  /// Total number of samples.
  len: f64,
}
impl CData {
  /// Constructor.
  #[inline]
  pub fn new(pos: HSamples, neg: HSamples, unc: HSamples) -> Self {
    let len = (
      pos.len() + neg.len() + unc.len()
    ) as f64 ;
    CData {
      pos, neg, unc, len
    }
  }

  /// Number of samples.
  #[inline]
  pub fn len(& self) -> usize { self.len as usize }

  /// True if the data is empty.
  #[inline]
  pub fn is_empty(& self) -> bool { self.len == 0. }

  /// Destroys the data.
  #[inline]
  pub fn destroy(self) -> (HSamples, HSamples, HSamples) {
    (self.pos, self.neg, self.unc)
  }

  /// Adds a positive sample.
  #[inline]
  pub fn add_pos(& mut self, pos: HSample) {
    self.len += 1. ;
    self.pos.push(pos)
  }
  /// Positive samples.
  #[inline]
  pub fn pos(& self) -> & HSamples {
    & self.pos
  }

  /// Adds a negative sample.
  #[inline]
  pub fn add_neg(& mut self, neg: HSample) {
    self.len += 1. ;
    self.neg.push(neg)
  }
  /// Negative samples.
  #[inline]
  pub fn neg(& self) -> & HSamples {
    & self.neg
  }

  /// Adds a unclassified sample.
  #[inline]
  pub fn add_unc(& mut self, unc: HSample) {
    self.len += 1. ;
    self.unc.push(unc)
  }
  /// Unclassified samples.
  #[inline]
  pub fn unc(& self) -> & HSamples {
    & self.unc
  }

  /// Classifies its unclassified elements.
  ///
  /// Function `f` will be applied to each element `e`, and it will be moved
  ///
  /// - to `pos` if `f(e) == Some(true)`,
  /// - to `neg` if `f(e) == Some(false)`,
  /// - nowhere if `f(e).is_some()`.
  #[inline]
  pub fn classify<F>(& mut self, mut f: F)
  where F: FnMut(& HSample) -> Option<bool> {
    let mut cnt = 0 ;
    while cnt < self.unc.len() {
      if let Some(pos) = f(& self.unc[cnt]) {
        let sample = self.unc.swap_remove(cnt) ;
        if pos {
          self.add_pos(sample)
        } else {
          self.add_neg(sample)
        }
      } else {
        cnt += 1
      }
    }
  }

  /// Shannon entropy given the number of positive and negative samples.
  fn shannon_entropy(pos: f64, neg: f64) -> f64 {
    let den = pos + neg ;
    if den == 0. { return 1. }
    let (pos, neg) = (pos / den, neg / den) ;
    let (pos, neg) = (
      if pos <= 0. { 0. } else { - ( pos * pos.log2() ) },
      if neg <= 0. { 0. } else { - ( neg * neg.log2() ) }
    ) ;
    pos + neg
  }

  /// Shannon-entropy-based information gain of a qualifier (simple, ignores
  /// unclassified data).
  pub fn simple_gain(& self, qual: & mut Qual) -> Res< Option<f64> > {
    let my_entropy = Self::shannon_entropy(
      self.pos.len() as f64, self.neg.len() as f64
    ) ;
    let card = (self.pos.len() as f64) + (self.neg.len() as f64) ;
    let (
      mut q_pos, mut q_neg, mut q_unc, mut nq_pos, mut nq_neg, mut nq_unc
    ) = (0., 0., 0., 0., 0., 0.) ;
    for pos in & self.pos {
      match qual.bool_eval( pos.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual.qual, pos)
      ) ? {
        Some(true) => q_pos += 1.,
        Some(false) => nq_pos += 1.,
        None => return Ok(None),
      }
    }
    for neg in & self.neg {
      match qual.bool_eval( neg.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual.qual, neg)
      ) ? {
        Some(true) => q_neg += 1.,
        Some(false) => nq_neg += 1.,
        None => return Ok(None),
      }
    }
    for unc in & self.unc {
      match qual.bool_eval( unc.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual.qual, unc)
      ) ? {
        Some(true) => q_unc += 1.,
        Some(false) => nq_unc += 1.,
        None => return Ok(None),
      }
    }
    if q_pos + q_neg + q_unc == 0. || nq_pos + nq_neg + nq_unc == 0. {
      Ok( None )
    } else {
      let (q_entropy, nq_entropy) = (
        Self::shannon_entropy( q_pos,  q_neg),
        Self::shannon_entropy(nq_pos, nq_neg)
      ) ;

      Ok(
        Some((
          my_entropy - (
            ( (q_pos + q_neg) *  q_entropy / card ) +
            ( (nq_pos + nq_neg) * nq_entropy / card )
          )
        ))
      )
    }
  }


  /// Modified entropy, uses [`EntropyBuilder`](struct.EntropyBuilder.html).
  ///
  /// Only takes into account unclassified data when `conf.ice.simple_gain`
  /// is false.
  pub fn entropy(& self, pred: PrdIdx, data: & DataCore) -> Res<f64> {
    let mut proba = EntropyBuilder::new() ;
    proba.set_pos_count( self.pos.len() ) ;
    proba.set_neg_count( self.neg.len() ) ;
    for unc in & self.unc {
      proba.add_unc(data, pred, unc) ?
    }
    proba.entropy()
  }

  /// Modified gain, uses `entropy`.
  ///
  /// Only takes into account unclassified data when `conf.ice.simple_gain`
  /// is false.
  pub fn gain<Trm: CanBEvaled>(
    & self, pred: PrdIdx, data: & DataCore, qual: & Trm, _profiler: & Profiler
  ) -> Res< Option<f64> > {
    let my_entropy = self.entropy(pred, data) ? ;

    let (mut q_ent, mut nq_ent) = (
      EntropyBuilder::new(), EntropyBuilder::new()
    ) ;
    let (
      mut q_pos, mut q_neg, mut q_unc, mut nq_pos, mut nq_neg, mut nq_unc
    ) = (0, 0, 0., 0, 0, 0.) ;

    profile! { |_profiler| tick "learning", "qual", "gain", "pos eval" }
    for pos in & self.pos {
      match qual.evaluate( pos.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual, pos)
      ) ? {
        Some(true) => q_pos += 1,
        Some(false) => nq_pos += 1,
        None => return Ok(None),
      }
    }
    q_ent.set_pos_count(q_pos) ;
    nq_ent.set_pos_count(nq_pos) ;
    profile! { |_profiler| mark "learning", "qual", "gain", "pos eval" }

    profile! { |_profiler| tick "learning", "qual", "gain", "neg eval" }
    for neg in & self.neg {
      match qual.evaluate( neg.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual, neg)
      ) ? {
        Some(true) => q_neg += 1,
        Some(false) => nq_neg += 1,
        None => return Ok(None),
      }
    }
    q_ent.set_neg_count(q_neg) ;
    nq_ent.set_neg_count(nq_neg) ;
    profile! { |_profiler| mark "learning", "qual", "gain", "neg eval" }

    profile! { |_profiler| tick "learning", "qual", "gain", "unc eval" }
    for unc in & self.unc {
      match qual.evaluate( unc.get() ).chain_err(
        || format!("while evaluating qualifier {} on {}", qual, unc)
      ) ? {
        Some(true) => {
          q_unc += 1. ;
          q_ent.add_unc(data, pred, unc) ?
        },
        Some(false) => {
          nq_unc += 1. ;
          nq_ent.add_unc(data, pred, unc) ?
        },
        None => return Ok(None),
      }
    }
    profile! { |_profiler| mark "learning", "qual", "gain", "unc eval" }


    profile! { |_profiler| tick "learning", "qual", "gain", "rest" }

    let (q_pos, q_neg, nq_pos, nq_neg) = (
      q_pos as f64, q_neg as f64, nq_pos as f64, nq_neg as f64
    ) ;

    let  q_sum =  q_pos +  q_neg +  q_unc ;
    let nq_sum = nq_pos + nq_neg + nq_unc ;

    // Is this qualifier separating anything?
    if  q_sum == 0. || nq_sum == 0. {
      profile! { |_profiler| mark "learning", "qual", "gain", "rest" }
      return Ok(None)
    }

    let (q_entropy, nq_entropy) = (
      q_ent.entropy() ?, nq_ent.entropy() ?
    ) ;

    let gain = my_entropy - (
       q_sum *  q_entropy / self.len +
      nq_sum * nq_entropy / self.len
    ) ;

    debug_assert! { ! gain.is_nan() }
  //   if gain.is_nan() {
  //     bail!(
  //       format!(
  //         "gain is NaN :(
  // my_entropy: {}
  // my_card: {}
  // q  numerator: {} * {} = {}
  // nq numerator: {} * {} = {}", my_entropy, self.len,
  //         q_sum,
  //         q_entropy,
  //         q_sum * q_entropy,
  //         nq_sum,
  //         nq_entropy,
  //         nq_sum * nq_entropy,
  //       )
  //     )
  //   }
    profile! { |_profiler| mark "learning", "qual", "gain", "rest" }

    Ok( Some(gain) )
  }

  /// Splits the data given some qualifier. First is the data for which the
  /// qualifier is true.
  pub fn split(self, qual: & Term) -> (Self, Self) {
    let (mut q, mut nq) = (
      CData::new(
        Vec::with_capacity( self.pos.len() ),
        Vec::with_capacity( self.neg.len() ),
        Vec::with_capacity( self.unc.len() ),
      ),
      CData::new(
        Vec::with_capacity( self.pos.len() ),
        Vec::with_capacity( self.neg.len() ),
        Vec::with_capacity( self.unc.len() ),
      )
    ) ;

    for pos in self.pos {
      if qual.bool_eval( pos.get() ).and_then(
        |res| res.ok_or_else(
          || ErrorKind::Msg( "model is not complete enough".into() ).into()
        )
      ).expect("error evaluating qualifier") {
        q.add_pos( pos )
      } else {
        nq.add_pos( pos )
      }
    }
    for neg in self.neg {
      if qual.bool_eval( neg.get() ).and_then(
        |res| res.ok_or_else(
          || ErrorKind::Msg( "model is not complete enough".into() ).into()
        )
      ).expect("error evaluating qualifier") {
        q.add_neg( neg )
      } else {
        nq.add_neg( neg )
      }
    }
    for unc in self.unc {
      if qual.bool_eval( unc.get() ).and_then(
        |res| res.ok_or_else(
          || ErrorKind::Msg( "model is not complete enough".into() ).into()
        )
      ).expect("error evaluating qualifier") {
        q.add_unc( unc )
      } else {
        nq.add_unc( unc )
      }
    }

    q.pos.shrink_to_fit() ;
    q.neg.shrink_to_fit() ;
    q.unc.shrink_to_fit() ;
    nq.pos.shrink_to_fit() ;
    nq.neg.shrink_to_fit() ;
    nq.unc.shrink_to_fit() ;

    (q, nq)
  }

  /// Iterator over some data.
  pub fn iter<'a>(& 'a self) -> CDataIter<'a> {
    CDataIter {
      pos: self.pos.iter(),
      neg: self.neg.iter(),
      unc: self.unc.iter(),
    }
  }
}

/// Iterator over CData.
pub struct CDataIter<'a> {
  pos: ::std::slice::Iter<'a, HSample>,
  neg: ::std::slice::Iter<'a, HSample>,
  unc: ::std::slice::Iter<'a, HSample>,
}
impl<'a> ::std::iter::Iterator for CDataIter<'a> {
  type Item = & 'a HSample ;
  fn next(& mut self) -> Option<Self::Item> {
    let next = self.pos.next() ;
    if next.is_some() { return next }
    let next = self.neg.next() ;
    if next.is_some() { return next }
    self.unc.next()
  }
}



/// Wrapper around an `f64` used to compute an approximation of the ratio
/// between legal positive classifications and negative ones, without actually
/// splitting the data.
///
/// See the paper for more details.
pub struct EntropyBuilder { num: f64, den: usize }
impl EntropyBuilder {
  /// Constructor.
  pub fn new() -> Self {
    EntropyBuilder { num: 0., den: 0 }
  }

  /// Sets the number of positive samples.
  pub fn set_pos_count(& mut self, pos: usize) {
    self.num += pos as f64 ;
    self.den += pos
  }
  /// Sets the number of negative samples.
  pub fn set_neg_count(& mut self, neg: usize) {
    self.den += neg
  }

  /// Adds the degree of an unclassified example.
  pub fn add_unc(
    & mut self, data: & DataCore, prd: PrdIdx, sample: & HSample
  ) -> Res<()> {
    let degree = Self::degree(data, prd, sample) ? ;
    self.den += 1 ;
    self.num += (1. / 2.) + (
      degree
    ).atan() / ::std::f64::consts::PI ;
    Ok(())
  }

  /// Probability stored in the builder.
  pub fn proba(& self) -> f64 {
    self.num / (self.den as f64)
  }

  /// Destroys the builder and returns the entropy.
  pub fn entropy(self) -> Res<f64> {
    let proba = self.proba() ;
    let (pos, neg) = (
      if proba == 0. { 0. } else {
        proba * proba.log2()
      },
      if proba == 1. { 0. } else {
        (1. - proba) * (1. - proba).log2()
      }
    ) ;
    let res = - pos - neg ;
    if res.is_nan() {
      bail!(
        format!(
          "entropy is NaN :(
  num  : {}
  den  : {}
  proba: {}
  pos  : {}
  neg  : {}", self.num, self.den, proba, pos, neg
        )
      )
    }
    Ok(res)
  }

  /// Degree of a sample, refer to the paper for details.
  pub fn degree(
    data: & DataCore, prd: PrdIdx, sample: & HSample
  ) -> Res<f64> {
    let (
      mut sum_imp_rhs,
      mut sum_imp_lhs,
      mut sum_neg,
    ) = (0., 0., 0.) ;

    if let Some(constraints) = data.map[prd].get(& sample) {
      for constraint in constraints {
        let constraint = & data.constraints[* constraint] ;
        match constraint.rhs {
          None => sum_neg = sum_neg + 1. / (constraint.lhs.len() as f64),
          Some( Sample { pred, ref args } )
          if pred == prd
          && args == sample => sum_imp_rhs = sum_imp_rhs + 1. / (
            1. + (constraint.lhs.len() as f64)
          ),
          _ => {
            debug_assert! {
              constraint.lhs.iter().fold(
                false,
                |b, (pred, samples)| b || samples.iter().fold(
                  b, |b, s| b || (
                    * pred == prd && s == sample
                  )
                )
              )
            }
            sum_imp_lhs = sum_imp_lhs + 1. / (
              1. + (constraint.lhs.len() as f64)
            )
          },
        }
      }
    }

    let res = sum_imp_rhs - sum_imp_lhs - sum_neg ;

    Ok(res)
  }
}