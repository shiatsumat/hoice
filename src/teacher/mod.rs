#![doc = r#"The teacher. Queries an SMT-solver to check candidates.

# TO DO

- clean `teach` function, it's a mess and the way it's currently written
  doesn't make sense

"#]

use rsmt2::{ ParseSmt2, Kid } ;

use nom::IResult ;

use common::* ;
use common::data::* ;
use common::msg::* ;




/// Starts the teaching process.
pub fn start_class(
  instance: & Arc<Instance>, profiler: & Profiler
) -> Res< Option<Candidates> > {
  use rsmt2::solver ;
  let instance = instance.clone() ;
  log_debug!{ "starting the learning process\n  launching solver kid..." }
  let mut kid = Kid::new( conf.solver.conf() ).chain_err(
    || ErrorKind::Z3SpawnError
  ) ? ;
  let res = {
    let solver = solver(& mut kid, Parser).chain_err(
      || "while constructing the teacher's solver"
    ) ? ;
    if let Some(log) = conf.solver.log_file("teacher") ? {
      teach( instance, solver.tee(log), profiler )
    } else {
      teach( instance, solver, profiler )
    }
  } ;

  kid.kill().chain_err(
    || "while killing solver kid"
  ) ? ;
  res
}


/// Teaching to the learners.
fn teach< 'kid, S: Solver<'kid, Parser> >(
  instance: Arc<Instance>, solver: S, profiler: & Profiler
) -> Res< Option<Candidates> > {
  log_debug!{ "  creating teacher" }
  let mut teacher = Teacher::new(solver, instance, profiler) ;

  // if conf.smt_learn {
  //   log_debug!{ "  spawning smt learner..." }
  //   teacher.add_learner( ::learning::smt::Launcher ) ?
  // }
  log_debug!{ "  spawning ice learner..." }
  teacher.add_learner( ::learning::ice::Launcher ) ? ;

  log_debug!{ "  performing initial check..." }
  let (cexs, cands) = teacher.initial_check() ? ;
  if cexs.is_empty() {
    teacher.finalize() ? ;
    return Ok( Some(cands) )
  }
  log_debug!{ "  generating data from initial cex..." }
  teacher.instance.cexs_to_data(& mut teacher.data, cexs ) ? ;

  log_debug!{ "  starting teaching loop" }
  'teach: loop {
    // log_info!{
    //   "all learning data:\n{}", teacher.data.string_do(
    //     & (), |s| s.to_string()
    //   ) ?
    // }

    if conf.teacher.step {
      let mut dummy = String::new() ;
      println!("") ;
      println!( "; {} to broadcast data...", conf.emph("press return") ) ;
      let _ = ::std::io::stdin().read_line(& mut dummy) ;
    }

    let one_alive = teacher.broadcast() ;
    if ! one_alive {
      bail!("all learners are dead")
    }

    match teacher.get_candidates() ? {

      // Unsat result, done.
      Some( (_idx, None) ) => {
        log_info!(
          "\ngot unsat result from {} learner", teacher.learners[_idx].1
        ) ;
        teacher.finalize() ? ;
        return Ok(None)
      },

      // Got a candidate.
      Some( ( _idx, Some(candidates) ) ) => {
        if_verb!{
          log_info!(
            "\nCurrent candidates from {} learner:",
            conf.emph( & teacher.learners[_idx].1 )
          ) ;
          for _pred in teacher.instance.preds() {
            log_info!("{}:", _pred.name) ;
            if let Some(term) = candidates[_pred.idx].as_ref() {
              log_info!("  {}", term)
            }
          }
        }
        profile!{ teacher tick "cexs" }
        let cexs = teacher.get_cexs(& candidates) ? ;
        profile!{ teacher mark "cexs" }

        if cexs.is_empty() {
          teacher.finalize() ? ;
          return Ok( Some(candidates) )
        }

        // log_info!{
        //   "\nlearning data before adding cex:\n{}",
        //   teacher.data.string_do(
        //     & (), |s| s.to_string()
        //   ) ?
        // }
        profile!{ teacher tick "data", "registration" }
        if let Err(e) = teacher.instance.cexs_to_data(
          & mut teacher.data, cexs
        ) {
          match e.kind() {
            & ErrorKind::Unsat => {
              teacher.finalize() ? ;
              return Ok(None)
            },
            _ => bail!(e),
          }
        }
        profile!{ teacher mark "data", "registration" }
        // log_info!{
        //   "\nlearning data before propagation:\n{}",
        //   teacher.data.string_do(
        //     & (), |s| s.to_string()
        //   ) ?
        // }
        profile!{ teacher tick "data", "propagation" }
        teacher.data.propagate() ? ;
        profile!{ teacher mark "data", "propagation" }
      },

      // Channel is dead.
      None => bail!("all learners are dead"),
    }
  }
}





/// The teacher, stores a solver.
pub struct Teacher<'a, S> {
  /// The solver.
  pub solver: S,
  /// The (shared) instance.
  pub instance: Arc<Instance>,
  /// Learning data.
  pub data: Data,
  /// Receiver.
  pub from_learners: Receiver<(LrnIdx, FromLearners)>,
  /// Sender used by learners. Becomes `None` when the learning process starts.
  pub to_teacher: Option< Sender<(LrnIdx, FromLearners)> >,
  /// Learners sender and description.
  pub learners: LrnMap<( Option< Sender<Data> >, String )>,
  /// Profiler.
  pub _profiler: & 'a Profiler,
  /// Number of guesses.
  count: usize,
}
impl<'a, 'kid, S: Solver<'kid, Parser>> Teacher<'a, S> {
  /// Constructor.
  pub fn new(
    solver: S, instance: Arc<Instance>, profiler: & 'a Profiler
  ) -> Self {
    let learners = LrnMap::with_capacity( 2 ) ;
    let (to_teacher, from_learners) = from_learners() ;
    let data = Data::new( instance.clone() ) ;
    Teacher {
      solver, instance, data, from_learners,
      to_teacher: Some(to_teacher), learners,
      _profiler: profiler, count: 0,
    }
  }

  /// Finalizes the run, does nothing in bench mode.
  #[cfg( not(feature = "bench") )]
  pub fn finalize(mut self) -> Res<()> {
    if conf.stats {
      println!("; Done in {} guess(es)", self.count) ;
      println!("") ;
    }
    for & mut (ref mut sender, _) in self.learners.iter_mut() {
      * sender = None
    }
    while self.get_candidates()?.is_some() {}
    Ok(())
  }
  /// Finalizes the run, does nothing in bench mode.
  #[cfg(feature = "bench")]
  #[inline(always)]
  pub fn finalize(self) -> Res<()> { Ok(()) }

  /// Adds a new learner.
  pub fn add_learner<L: Learner + 'static>(& mut self, learner: L) -> Res<()> {
    if let Some(to_teacher) = self.to_teacher.clone() {
      let index = self.learners.next_index() ;
      let name = learner.description() ;
      let instance = self.instance.clone() ;
      let data = self.data.clone() ;
      let (to_learner, learner_recv) = new_to_learner() ;
      ::std::thread::Builder::new().name( name.clone() ).spawn(
        move || learner.run(
          LearnerCore::new(index, to_teacher.clone(), learner_recv),
          instance, data
        )
      ).chain_err(
        || format!("while spawning learner `{}`", conf.emph(& name))
      ) ? ;
      self.learners.push( ( Some(to_learner), name ) ) ;
      Ok(())
    } else {
      bail!("trying to add learner after teacher's finalization")
    }
  }

  /// Broadcasts data to the learners. Returns `true` if there's no more
  /// learner left.
  pub fn broadcast(& self) -> bool {
    profile!{ self tick "broadcast" }
    let mut one_alive = false ;
    log_info!{ "broadcasting..." }
    for & (ref sender, ref name) in self.learners.iter() {
      if let Some(sender) = sender.as_ref() {
        if let Err(_) = sender.send( self.data.clone() ) {
          warn!( "learner `{}` is dead...", name )
        } else {
          one_alive = true
        }
      }
    }
    log_info!{ "done broadcasting..." }
    profile!{ self mark "broadcast" }
    one_alive
  }

  /// Waits for some candidates.
  ///
  /// Returns `None` when there are no more kids. Otherwise, the second
  /// element of the pair is `None` if a learner concluded `unsat`, and
  /// `Some` of the candidates otherwise.
  pub fn get_candidates(
    & self
  ) -> Res< Option<(LrnIdx, Option<Candidates>)> > {
    profile!{ self tick "waiting" }
    'recv: loop {
      match self.from_learners.recv() {
        Ok( (_idx, FromLearners::Msg(_s)) ) => if_verb!{
          for _line in _s.lines() {
            log_info!(
              "{} > {}", conf.emph( & self.learners[_idx].1 ), _line
            )
          }
        },
        Ok( (idx, FromLearners::Err(e)) ) => {
          let err: Res<()> = Err(e) ;
          let err: Res<()> = err.chain_err(
            || format!(
              "from {} learner", conf.emph( & self.learners[idx].1 )
            )
          ) ;
          // println!("receiving:") ;
          // for err in e.iter() {
          //   println!("{}", err)
          // }
          print_err( err.unwrap_err() )
        },
        Ok( (idx, FromLearners::Stats(tree, stats)) ) => if conf.stats {
          println!(
            "; received stats from {}", conf.emph( & self.learners[idx].1 )
          ) ;
          tree.print() ;
          if ! stats.is_empty() {
            println!("; stats:") ;
            stats.print()
          }
          println!("")
        },
        Ok( (idx, FromLearners::Cands(cands)) ) => {
          profile!{ self mark "waiting" }
          profile!{ self "candidates" => add 1 }
          return Ok( Some( (idx, Some(cands)) ) )
        },
        Ok( (idx, FromLearners::Unsat) ) => {
          return Ok( Some( (idx, None) ) )
        },
        Err(_) => {
          profile!{ self mark "waiting" }
          return Ok( None )
        },
      }
    }
  }

  /// Initial check, where all candidates are `true`.
  ///
  /// Drops the copy of the `Sender` end of the channel used to communicate
  /// with the teacher (`self.to_teacher`). This entails that attempting to
  /// receive messages will automatically fail if all learners are dead.
  pub fn initial_check(& mut self) -> Res< (Cexs, Candidates) > {
    // Drop `to_teacher` sender so that we know when all kids are dead.
    self.to_teacher = None ;

    let mut cands = PrdMap::with_capacity( self.instance.preds().len() ) ;
    for pred in self.instance.pred_indices() {
      if self.instance.forced_terms_of(pred).is_some() {
        cands.push( None )
      } else {
        cands.push( Some(term::tru()) )
      }
    }
    self.get_cexs(& cands).map(|res| (res, cands))
  }

  /// Looks for falsifiable clauses given some candidates.
  pub fn get_cexs(& mut self, cands: & Candidates) -> Res< Cexs > {
    use std::iter::Extend ;
    self.count += 1 ;
    self.solver.reset() ? ;

    // These will be passed to clause printing to inline trivial predicates.
    let (mut true_preds, mut false_preds) = ( PrdSet::new(), PrdSet::new() ) ;
    // Clauses to ignore, because they are trivially true. (lhs is false or
    // rhs is true).
    let mut clauses_to_ignore = ClsSet::new() ;

    // Define non-forced predicates that are not trivially true or false.
    'define_non_forced: for (pred, cand) in cands.index_iter() {
      if let Some(ref term) = * cand {
        match term.bool() {
          Some(true) => {
            let _ = true_preds.insert(pred) ;
            clauses_to_ignore.extend(
              self.instance.clauses_of(pred).1
            )
          },
          Some(false) => {
            let _ = false_preds.insert(pred) ;
            clauses_to_ignore.extend(
              self.instance.clauses_of(pred).0
            )
          },
          None => {
            let pred = & self.instance[pred] ;
            let sig: Vec<_> = pred.sig.index_iter().map(
              |(var, typ)| (var, * typ)
            ).collect() ;
            self.solver.define_fun(
              & pred.name, & sig, & Typ::Bool, & TermWrap(term), & ()
            ) ?
          },
        }
      }
    }

    // Define forced predicates in topological order.
    'forced_preds: for pred in self.instance.sorted_forced_terms() {
      let pred = * pred ;
      let tterms = if let Some(tterms) = self.instance.forced_terms_of(pred) {
        tterms
      } else {
        bail!(
          "inconsistency between forced predicates and \
          sorted forced predicates"
        )
      } ;

      match * tterms {
        TTerms::True => {
          true_preds.insert(pred) ;
        },
        TTerms::False => {
          false_preds.insert(pred) ;
        },
        _ => {
          let pred = & self.instance[pred] ;
          let sig: Vec<_> = pred.sig.index_iter().map(
            |(var, typ)| (var, * typ)
          ).collect() ;
          self.solver.define_fun(
            & pred.name, & sig, & Typ::Bool, tterms,
            & ( & true_preds, & false_preds, self.instance.preds() )
          ) ?
        },
      }

    }

    // // Define non-trivially true or false predicates.
    // 'define_preds: for (pred, cand) in cands.index_iter() {
    //   if let Some(term) = cand.as_ref() {
    //     match term.bool() {
    //       Some(true) => {
    //         let _ =  true_preds.insert(pred) ;
    //         clauses_to_ignore.extend(
    //           self.instance.clauses_of(pred).1
    //         )
    //       },
    //       Some(false) => {
    //         let _ = false_preds.insert(pred) ;
    //         clauses_to_ignore.extend(
    //           self.instance.clauses_of(pred).0
    //         )
    //       },
    //       None => {
    //         let pred = & self.instance[pred] ;
    //         let sig: Vec<_> = pred.sig.index_iter().map(
    //           |(var, typ)| (var, * typ)
    //         ).collect() ;
    //         self.solver.define_fun(
    //           & pred.name, & sig, & Typ::Bool, & TermWrap(term), & ()
    //         ) ?
    //       }
    //     }
    //   } else if let Some(tterms) = self.instance.forced_terms_of(pred) {
    //     if tterms.len() == 1 {
    //       match tterms[0].bool() {
    //         Some(true)  => {
    //           let _ =  true_preds.insert(pred) ;
    //           clauses_to_ignore.extend(
    //             self.instance.clauses_of(pred).1
    //           ) ;
    //           continue 'define_preds
    //         },
    //         Some(false) => {
    //           let _ = false_preds.insert(pred) ;
    //           clauses_to_ignore.extend(
    //             self.instance.clauses_of(pred).0
    //           ) ;
    //           continue 'define_preds
    //         },
    //         None => (),
    //       }
    //     }
    //     let pred = & self.instance[pred] ;
    //     let sig: Vec<_> = pred.sig.index_iter().map(
    //       |(var, typ)| (var, * typ)
    //     ).collect() ;
    //     self.solver.define_fun(
    //       & pred.name, & sig, & Typ::Bool, & TTermsWrap(tterms),
    //       self.instance.preds()
    //     ) ?
    //   } else {
    //     bail!("illegal incomplete candidates")
    //   }
    // }

    let mut map = ClsHMap::with_capacity( self.instance.clauses().len() ) ;
    let clauses = ClsRange::zero_to( self.instance.clauses().len() ) ;
    self.solver.comment("looking for counterexamples...") ? ;
    for clause in clauses {
      if ! clauses_to_ignore.contains(& clause) {
        // log_debug!{ "  looking for a cex for clause {}", clause }
        if let Some(cex) = self.get_cex(
          clause, & true_preds, & false_preds
        ).chain_err(
          || format!("while getting counterexample for clause {}", clause)
        ) ? {
          let prev = map.insert(clause, cex) ;
          debug_assert_eq!(prev, None)
        }
      }
    }
    Ok(map)
  }

  /// Checks if a clause is falsifiable and returns a model if it is.
  pub fn get_cex(
    & mut self, clause_idx: ClsIdx, true_preds: & PrdSet, false_preds: & PrdSet
  ) -> SmtRes< Option<Cex> > {
    self.solver.push(1) ? ;
    let clause = & self.instance[clause_idx] ;
    if_not_bench!{
      if conf.solver.log {
        self.solver.comment(& format!("clause variables:\n")) ? ;
        for info in clause.vars() {
          self.solver.comment(
            & format!("  v_{} ({})\n", info.idx, info.active)
          ) ?
        }
        self.solver.comment(& format!("lhs terms:\n")) ? ;
        for lhs in clause.lhs_terms() {
          self.solver.comment(
            & format!("  {}\n", lhs)
          ) ?
        }
        self.solver.comment(& format!("lhs pred applications:\n")) ? ;
        for (pred, argss) in clause.lhs_preds() {
          for args in argss {
            let mut s = format!("  ({}", & self.instance[* pred]) ;
            for arg in args {
              s = format!("{} {}", s, arg)
            }
            s.push(')') ;
            self.solver.comment(& s) ?
          }
        }
        self.solver.comment(& format!("rhs:\n")) ? ;
        self.solver.comment(
          & format!("  {}", clause.rhs())
        ) ?
      }
    }
    profile!{ self tick "cexs", "prep" }
    for var in clause.vars() {
      if var.active {
        self.solver.declare_const(& var.idx, & var.typ, & ()) ?
      }
    }
    self.solver.assert(
      clause, & (true_preds, false_preds, self.instance.preds())
    ) ? ;
    profile!{ self mark "cexs", "prep" }
    profile!{ self tick "cexs", "check-sat" }
    let sat = self.solver.check_sat() ? ;
    profile!{ self mark "cexs", "check-sat" }
    let res = if sat {
      profile!{ self tick "cexs", "model" }
      log_debug!{ "    sat, getting model..." }
      let model = self.solver.get_model() ? ;
      let mut map: VarMap<_> = clause.vars().iter().map(
        |info| info.typ.default_val()
      ).collect() ;
      for (var,val) in model {
        log_debug!{ "    - {} = {}", var.default_str(), val }
        map[var] = val
      }
      log_debug!{ "    done constructing model" }
      profile!{ self mark "cexs", "model" }
      Ok( Some( map ) )
    } else {
      Ok(None)
    } ;
    self.solver.pop(1) ? ;
    res
  }
}



/// Wraps a term to write as the body of a `define-fun`.
pub struct TermWrap<'a>( & 'a Term ) ;
impl<'a> ::rsmt2::Expr2Smt<()> for TermWrap<'a> {
  fn expr_to_smt2<Writer: Write>(
    & self, w: & mut Writer, _: & ()
  ) -> SmtRes<()> {
    let msg = "writing term as smt2" ;
    smt_cast_io!{
      msg => self.0.write(
        w, |w, var| var.default_write(w),
      )
    }
  }
}






#[doc = r#"Unit type parsing the output of the SMT solver.

Parses variables of the form `v<int>` and constants. It is designed to parse
models of the falsification of a single clause, where the variables of the
clause are written as `v<index>` in smt2.
"#]
pub struct Parser ;
impl ParseSmt2 for Parser {
  type Ident = VarIdx ;
  type Value = Val ;
  type Expr = () ;
  type Proof = () ;
  type I = () ;

  fn parse_ident<'a>(
    & self, bytes: & 'a [u8]
  ) -> IResult<& 'a [u8], VarIdx> {
    use std::str::FromStr ;
    preceded!(
      bytes,
      tag!("v_"),
      map!(
        map_res!(
          map_res!(
            re_bytes_find!("^[0-9][0-9]*"),
            ::std::str::from_utf8
          ),
          usize::from_str
        ),
        |i| i.into()
      )
    )
  }

  fn parse_value<'a>(
    & self, bytes: & 'a [u8]
  ) -> IResult<& 'a [u8], Val> {
    fix_error!( bytes, u32, call!(Val::parse) )
  }

  fn parse_expr<'a>(
    & self, _: & 'a [u8], _: & ()
  ) -> IResult<& 'a [u8], ()> {
    panic!("[bug] `parse_expr` of the teacher parser should never be called")
  }

  fn parse_proof<'a>(
    & self, _: & 'a [u8]
  ) -> IResult<& 'a [u8], ()> {
    panic!("[bug] `parse_proof` of the teacher parser should never be called")
  }
}


