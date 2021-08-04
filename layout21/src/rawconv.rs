//! # Raw-Layout Conversion Module

use super::*;
use layout21raw as raw;

/// A short-lived set of references to an [Instance] and its cell-definition
#[derive(Debug, Clone)]
struct TempInstance<'a> {
    inst: &'a Instance,
    def: &'a (dyn HasOutline + 'static),
}
// Create key-types for each internal type stored in [SlotMap]s
new_key_type! {
    /// Keys for [ValidAssign] entries
    pub struct AssignKey;
}
/// A short-lived Cell, largely organized by layer
#[derive(Debug, Clone)]
struct TempCell<'a> {
    /// Reference to the source [Cell]
    cell: &'a Cell,
    /// Reference to the source [Library]
    lib: &'a Library,
    /// Instances and references to their definitions
    instances: Vec<TempInstance<'a>>,
    /// Cuts, arranged by Layer
    cuts: Vec<Vec<validate::ValidTrackLoc>>,
    /// Validated Assignments
    assignments: SlotMap<AssignKey, validate::ValidAssign>,
    /// Assignments, arranged by Layer
    top_assns: Vec<Vec<AssignKey>>,
    /// Assignments, arranged by Layer
    bot_assns: Vec<Vec<AssignKey>>,
}
/// Temporary arrangement of data for a [Layer] within a [Cell]
#[derive(Debug, Clone)]
struct TempCellLayer<'a> {
    /// Reference to the validated metal layer
    layer: &'a validate::ValidMetalLayer,
    /// Reference to the parent cell
    cell: &'a TempCell<'a>,
    /// Instances which intersect with this layer and period
    instances: Vec<&'a TempInstance<'a>>,
    /// Pitch per layer-period
    pitch: DbUnits,
    /// Number of layer-periods
    nperiods: usize,
    /// Spanning distance in the layer's "infinite" dimension
    span: DbUnits,
}

/// Short-Lived structure of the stuff relevant for converting a single LayerPeriod,
/// on a single Layer, in a single Cell.
#[derive(Debug, Clone)]
struct TempPeriod<'a> {
    periodnum: usize,
    cell: &'a TempCell<'a>,
    layer: &'a TempCellLayer<'a>,
    /// Instance Blockages
    blockages: Vec<(PrimPitches, PrimPitches)>,
    cuts: Vec<&'a validate::ValidTrackLoc>,
    top_assns: Vec<AssignKey>,
    bot_assns: Vec<AssignKey>,
}
/// # Converter from [Library] and constituent elements to [raw::Library]
pub struct RawConverter {
    pub(crate) lib: Library,
    pub(crate) stack: validate::ValidStack,
    pub(crate) rawlayers: raw::Layers,
    pub(crate) layerkeys: Vec<raw::LayerKey>,
    pub(crate) boundary_layer: raw::LayerKey,
}
impl RawConverter {
    /// Convert [Library] `lib` to a [raw::Library]
    /// Consumes `lib` in the process
    pub fn convert(lib: Library, stack: Stack) -> LayoutResult<raw::Library> {
        let stack = validate::StackValidator::validate(stack)?;
        let (mut rawlayers, layerkeys) = Self::convert_layers(&stack)?;
        // Conversion requires our [Stack] specify a `boundary_layer`. If not, fail.
        let boundary_layer = (stack.boundary_layer).as_ref().ok_or(LayoutError::msg(
            "Cannot Convert Abstract to Raw without Boundary Layer",
        ))?;
        let boundary_layer = rawlayers.add(boundary_layer.clone());
        Self {
            lib,
            stack,
            rawlayers,
            layerkeys,
            boundary_layer,
        }
        .convert_lib()
    }
    /// Convert our [Stack] to [raw::Layers]
    fn convert_layers(
        stack: &validate::ValidStack,
    ) -> LayoutResult<(raw::Layers, Vec<raw::LayerKey>)> {
        let mut rawlayers = raw::Layers::default();
        let mut layerkeys = Vec::new();
        for layer in stack.metals.iter() {
            let rawlayer = match &layer.spec.raw {
                Some(r) => r.clone(), // FIXME: stop cloning
                None => return Err("Error exporting: no raw layer defined".into()),
            };
            let key = rawlayers.add(rawlayer);
            layerkeys.push(key);
        }
        Ok((rawlayers, layerkeys))
    }
    /// Convert everything in our [Library]
    /// FIXME: do we need to consume `self`?
    fn convert_lib(self) -> LayoutResult<raw::Library> {
        let mut lib = raw::Library::new(&self.lib.name, self.stack.units);
        lib.layers = self.rawlayers.clone();
        // Convert each defined [Cell] to a [raw::Cell]
        for (_id, cell) in self.lib.cells.iter() {
            lib.cells.push(self.convert_cell(cell)?);
        }
        // And convert each (un-implemented) Abstract as a boundary
        for (_id, abs) in self.lib.abstracts.iter() {
            // FIXME: temporarily checking whether the same name is already defined
            for (_id, cell) in self.lib.cells.iter() {
                if abs.name == cell.name {
                    continue;
                }
            }
            lib.cells.push(self.convert_abstract(abs)?);
        }
        Ok(lib)
    }
    /// Convert to a raw layout cell
    fn convert_cell(&self, cell: &Cell) -> LayoutResult<raw::Cell> {
        if cell.outline.x.len() > 1 {
            return Err(LayoutError::Str(
                "Non-rectangular outline; conversions not supported (yet)".into(),
            ));
        };
        let mut elems: Vec<raw::Element> = Vec::new();
        // Re-organize the cell into the format most helpful here
        let temp_cell = self.temp_cell(cell)?;
        // Convert a layer at a time, starting from bottom
        for layernum in 0..cell.top_layer {
            // Organize the cell/layer combo into temporary conversion format
            let temp_layer = self.temp_cell_layer(&temp_cell, &self.stack.metals[layernum]);
            // Convert each "layer period" one at a time
            for periodnum in 0..temp_layer.nperiods {
                // Again, re-organize into the relevant objects for this "layer period"
                let temp_period = self.temp_cell_layer_period(&temp_layer, periodnum);
                // And finally start doing stuff!
                elems.extend(self.convert_cell_layer_period(&temp_period)?);
            }
        }
        // Convert our [Outline] into a polygon
        elems.push(self.convert_outline(cell)?);
        // Convert our [Instance]s
        let insts = cell
            .instances
            .iter()
            .map(|inst| self.convert_instance(inst).unwrap()) // FIXME: error handling
            .collect();
        // Aaaand create & return our new [raw::Cell]
        Ok(raw::Cell {
            name: cell.name.clone(),
            insts,
            elems,
            ..Default::default()
        })
    }
    /// Create a [TempCell], organizing [Cell] data in more-convenient fashion for conversion
    fn convert_instance(&self, inst: &Instance) -> LayoutResult<raw::Instance> {
        // Primarily scale the location of each instance by our pitches
        Ok(raw::Instance {
            inst_name: inst.inst_name.clone(),
            // FIXME: both these fields
            cell_name: inst.cell_name.clone(),
            cell: inst.cell.clone(),
            p0: self.convert_xy(&inst.loc),
            reflect: inst.reflect,
            angle: inst.angle,
        })
    }
    /// Create a [TempCell], organizing [Cell] data in more-convenient fashion for conversion
    fn temp_cell<'a>(&'a self, cell: &'a Cell) -> LayoutResult<TempCell<'a>> {
        // Create one of these for each of our instances
        let instances: Vec<TempInstance> = cell
            .instances
            .iter()
            .map(|inst| match inst.cell {
                CellRef::Cell(c) => {
                    let def = self.lib.cells.get(c).ok_or(LayoutError::Tbd).unwrap();
                    Ok(TempInstance { inst, def })
                }
                CellRef::Abstract(c) => {
                    let def = self.lib.abstracts.get(c).ok_or(LayoutError::Tbd).unwrap();
                    Ok(TempInstance { inst, def })
                }
                _ => {
                    return Err(LayoutError::from("FIXME?"));
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Validate `cuts`, and arrange them by layer
        let mut cuts: Vec<Vec<validate::ValidTrackLoc>> = vec![vec![]; cell.top_layer()];
        for cut in cell.cuts.iter() {
            let c = validate::ValidTrackLoc::validate(cut, &self.stack)?;
            cuts[c.layer].push(c);
            // FIXME: cell validation should also check that this lies within our outline. probably do this earlier
        }
        // Validate all the cell's assignments, and arrange references by layer
        let mut bot_assns = vec![vec![]; cell.top_layer()];
        let mut top_assns = vec![vec![]; cell.top_layer()];
        let mut assignments = SlotMap::with_key();
        for assn in cell.assignments.iter() {
            let v = validate::ValidAssign::validate(assn, &self.stack)?;
            let bot = v.bot.layer;
            let top = v.top.layer;
            let k = assignments.insert(v);
            bot_assns[bot].push(k);
            top_assns[top].push(k);
        }
        // And create our (temporary) cell data!
        Ok(TempCell {
            cell,
            lib: &self.lib,
            instances,
            assignments,
            top_assns,
            bot_assns,
            cuts,
        })
    }
    /// Convert a single row/col (period) on a single layer in a single Cell.
    fn convert_cell_layer_period(
        &self,
        temp_period: &TempPeriod,
    ) -> LayoutResult<Vec<raw::Element>> {
        let mut elems: Vec<raw::Element> = Vec::new();
        let layer = temp_period.layer.layer; // FIXME! Can't love this name.

        // Create the layer-period object we'll manipulate most of the way
        let mut layer_period = temp_period
            .layer
            .layer
            .spec
            .to_layer_period(temp_period.periodnum, temp_period.layer.span.0)?;
        // Insert blockages on each track
        for (n1, n2) in temp_period.blockages.iter() {
            // Convert primitive-pitch-based blockages to db units
            let start = self.convert_to_db_units(*n1);
            let stop = self.convert_to_db_units(*n2);
            layer_period.cut(start, stop)?;
        }
        // Place all relevant cuts
        let nsig = layer_period.signals.len();
        for cut in temp_period.cuts.iter() {
            // Cut the assigned track
            let track = &mut layer_period.signals[cut.track & nsig];
            track.cut(
                cut.dist - layer.spec.cutsize / 2,
                cut.dist + layer.spec.cutsize / 2,
            )?;
        }
        // Handle Net Assignments
        // Start with those for which we're the lower of the two layers.
        // These will also be where we add vias
        let via_layer = &self.stack.vias[layer.index];
        for assn_id in temp_period.bot_assns.iter() {
            let assn = temp_period
                .cell
                .assignments
                .get(*assn_id)
                .ok_or(LayoutError::from("Internal error: invalid assignment"))?;
            // Grab a (mutable) reference to the assigned track
            let track = &mut layer_period.signals[assn.bot.track & nsig];
            // Find the segment corresponding to the off-axis coordinate
            let mut segment = track
                .segment_at(assn.bot.dist)
                .ok_or(LayoutError::msg("COULDNT FIND SEGMENT"))?;
            // Assign both track-segments to the net
            segment.net = Some(assn.net.clone());

            // FIXME: orientations will be wrong here
            let raw_layer = via_layer
                .raw
                .as_ref()
                .ok_or(LayoutError::msg("Raw-Layout Layer Not Defined"))?;
            let e = raw::Element {
                net: None,
                layer: self.layerkeys[layer.index],
                purpose: raw::LayerPurpose::Drawing,
                inner: raw::Shape::Rect {
                    p0: self.convert_point(
                        assn.bot.dist - via_layer.size.x / 2,
                        assn.top.dist - via_layer.size.y / 2,
                    ),
                    p1: self.convert_point(
                        assn.bot.dist + via_layer.size.x / 2,
                        assn.top.dist + via_layer.size.y / 2,
                    ),
                },
            };
            elems.push(e);
        }
        // Assign all the segments for which we're the top layer
        for assn_id in temp_period.top_assns.iter() {
            let assn = temp_period
                .cell
                .assignments
                .get(*assn_id)
                .ok_or(LayoutError::from("Internal error: invalid assignment"))?;
            // Grab a (mutable) reference to the assigned track
            let track = &mut layer_period.signals[assn.top.track & nsig];
            // Find the segment corresponding to the off-axis coordinate
            let mut segment = track
                .segment_at(assn.top.dist)
                .ok_or(LayoutError::from("Internal error: invalid segment"))?;
            // Assign both track-segments to the net
            segment.net = Some(assn.net.clone());
        }
        // Convert all TrackSegments to raw Elements
        for t in layer_period.rails.iter() {
            elems.extend(self.convert_track(t, &layer)?);
        }
        for t in layer_period.signals.iter() {
            elems.extend(self.convert_track(t, &layer)?);
        }
        Ok(elems)
    }
    /// Convert an [Abstract]
    /// FIXME: make these [raw::Abstract] instead
    pub fn convert_abstract(&self, abs: &abstrakt::Abstract) -> LayoutResult<raw::Cell> {
        let mut elems = Vec::with_capacity(abs.ports.len() + 1);
        // Create our [Outline]s boundary
        elems.push(self.convert_outline(abs)?);
        // Create shapes for each pin
        for port in abs.ports.iter() {
            // Add its shape
            use abstrakt::PortKind::{Edge, Zfull, Zlocs};

            let elem = match &port.kind {
                Edge {
                    layer: layer_index,
                    track,
                    side,
                } => {
                    let layer = &self.stack.metals[*layer_index].spec;
                    // First get the "infinite dimension" coordinate from the edge
                    let infdims: (DbUnits, DbUnits) = match side {
                        abstrakt::Side::BottomOrLeft => (DbUnits(0), DbUnits(100)),
                        abstrakt::Side::TopOrRight => {
                            // FIXME: this assumes rectangular outlines; will take some more work for polygons.
                            let outside = self.convert_to_db_units(abs.outline().max(layer.dir));
                            (outside - DbUnits(100), outside)
                        }
                    };
                    // Now get the "periodic dimension" from our layer-center
                    let perdims: (DbUnits, DbUnits) = self.track_span(*layer_index, *track)?;
                    // Presuming we're horizontal, points are here:
                    let mut pts = [Xy::new(infdims.0, perdims.0), Xy::new(infdims.1, perdims.1)];
                    // And if vertical, just transpose them
                    if layer.dir == Dir::Vert {
                        pts[0] = pts[0].transpose();
                        pts[1] = pts[1].transpose();
                    }
                    raw::Element {
                        net: Some(port.name.clone()),
                        layer: self.layerkeys[*layer_index],
                        purpose: raw::LayerPurpose::Drawing,
                        inner: raw::Shape::Rect {
                            p0: self.convert_xy(&pts[0]),
                            p1: self.convert_xy(&pts[1]),
                        },
                    }
                }
                Zlocs { .. } | Zfull { .. } => todo!(),
            };
            elems.push(elem);
        }
        // And return a new [raw::Abstract]
        Ok(raw::Cell {
            name: abs.name.clone(),
            elems,
            ..Default::default()
        })
    }
    /// Get the positions spanning track number `track` on layer number `layer`
    fn track_span(
        &self,
        layer_index: usize,
        track_index: usize,
    ) -> LayoutResult<(DbUnits, DbUnits)> {
        let layer = &self.stack.metals[layer_index];
        layer.span(track_index)
    }
    /// Convert an [Outline] to a [raw::Element] polygon
    pub fn convert_outline(&self, cell: &impl HasOutline) -> LayoutResult<raw::Element> {
        let outline: &Outline = cell.outline();
        // Create an array of Outline-Points
        let mut pts = vec![Point { x: 0, y: 0 }];
        let mut xp: isize;
        let mut yp: isize = 0;
        for i in 0..outline.x.len() {
            xp = self.convert_to_db_units(outline.x[i]).raw();
            pts.push(Point::new(xp, yp));
            yp = self.convert_to_db_units(outline.y[i]).raw();
            pts.push(Point::new(xp, yp));
        }
        // Add the final implied Point at (x, y[-1])
        pts.push(Point::new(0, yp));
        // Create the [raw::Element]
        Ok(raw::Element {
            net: None,
            layer: self.boundary_layer,
            purpose: raw::LayerPurpose::Outline,
            inner: raw::Shape::Poly { pts },
        })
    }
    /// Convert a [Track]-full of [TrackSegment]s to a vector of [raw::Element] rectangles
    fn convert_track(
        &self,
        track: &Track,
        layer: &validate::ValidMetalLayer,
    ) -> LayoutResult<Vec<raw::Element>> {
        let elems = track
            .segments
            .iter()
            .map(|seg| match track.dir {
                Dir::Horiz => raw::Element {
                    net: seg.net.clone(),
                    layer: self.layerkeys[layer.index],
                    purpose: raw::LayerPurpose::Drawing,
                    inner: raw::Shape::Rect {
                        p0: self.convert_point(seg.start, track.start),
                        p1: self.convert_point(seg.stop, track.start + track.width),
                    },
                },
                Dir::Vert => raw::Element {
                    net: seg.net.clone(),
                    layer: self.layerkeys[layer.index],
                    purpose: raw::LayerPurpose::Drawing,
                    inner: raw::Shape::Rect {
                        p0: self.convert_point(track.start, seg.start),
                        p1: self.convert_point(track.start + track.width, seg.stop),
                    },
                },
            })
            .collect();
        Ok(elems)
    }
    /// Create a [TempCellLayer] for the intersection of `temp_cell` and `layer`
    fn temp_cell_layer<'a>(
        &self,
        temp_cell: &'a TempCell,
        layer: &'a validate::ValidMetalLayer,
    ) -> TempCellLayer<'a> {
        // Sort out which of the cell's [Instance]s come up to this layer
        let instances: Vec<&TempInstance> = temp_cell
            .instances
            .iter()
            .filter(|i| i.def.top_layer() >= layer.index)
            .collect();

        // Sort out which direction we're working across
        let cell = temp_cell.cell;
        // Convert to database units
        let x = self.convert_to_db_units(cell.outline.x[0]); // FIXME: rectangles implied here
        let y = self.convert_to_db_units(cell.outline.y[0]);
        let (span, breadth) = match layer.spec.dir {
            Dir::Horiz => (x, y),
            Dir::Vert => (y, x),
        };

        // FIXME: we probably want to detect bad Outline dimensions sooner than this
        if (breadth % layer.pitch) != 0 {
            panic!("HOWD WE GET THIS BAD LAYER?!?!");
        }
        let nperiods = usize::try_from(breadth / layer.pitch).unwrap(); // FIXME: errors

        TempCellLayer {
            layer,
            cell: temp_cell,
            instances,
            nperiods,
            pitch: layer.pitch,
            span,
        }
    }
    /// Create the [TempPeriod] at the intersection of `temp_layer` and `periodnum`
    fn temp_cell_layer_period<'a>(
        &self,
        temp_layer: &'a TempCellLayer,
        periodnum: usize,
    ) -> TempPeriod<'a> {
        let cell = temp_layer.cell;
        let layer = temp_layer.layer;
        let dir = layer.spec.dir;

        // For each row, decide which instances intersect
        // Convert these into blockage-areas for the tracks
        let blockages = temp_layer
            .instances
            .iter()
            .filter(|i| self.instance_intersects(i, layer, periodnum))
            .map(|i| (i.inst.loc[dir], i.inst.loc[dir] + i.def.outline().max(dir)))
            .collect();

        // Grab indices of the relevant tracks for this period
        let nsig = temp_layer.layer.period.signals.len();
        let relevant_track_nums = (periodnum * nsig, (periodnum + 1) * nsig);
        // Filter cuts down to those in this period
        let cuts: Vec<&validate::ValidTrackLoc> = cell.cuts[temp_layer.layer.index]
            .iter()
            .filter(|cut| cut.track >= relevant_track_nums.0 && cut.track < relevant_track_nums.1)
            .collect();
        // Filter assignments down to those in this period
        let top_assns = cell.top_assns[temp_layer.layer.index]
            .iter()
            .filter(|id| {
                let assn = cell
                    .assignments
                    .get(**id)
                    .ok_or(LayoutError::from("Internal error: invalid assignment"))
                    .unwrap();
                assn.top.track >= relevant_track_nums.0 && assn.top.track < relevant_track_nums.1
            })
            .copied()
            .collect();
        let bot_assns = cell.bot_assns[temp_layer.layer.index]
            .iter()
            .filter(|id| {
                let assn = cell
                    .assignments
                    .get(**id)
                    .ok_or(LayoutError::from("Internal error: invalid assignment"))
                    .unwrap();
                assn.bot.track >= relevant_track_nums.0 && assn.bot.track < relevant_track_nums.1
            })
            .copied()
            .collect();

        TempPeriod {
            periodnum,
            cell,
            layer: temp_layer,
            blockages,
            cuts,
            top_assns,
            bot_assns,
        }
    }
    /// Boolean indication of whether `inst` intersects `layer` at `periodnum`
    fn instance_intersects(
        &self,
        inst: &TempInstance,
        layer: &validate::ValidMetalLayer,
        periodnum: usize,
    ) -> bool {
        // Grab the instance's [DbUnits] span, in the layer's periodic direction
        let dir = !layer.spec.dir;
        let inst_min = self.convert_to_db_units(inst.inst.loc[dir]);
        let inst_max = inst_min + self.convert_to_db_units(inst.def.outline().max(dir));
        // And return the boolean intersection. "Touching" edge-to-edge is *not* considered an intersection.
        return inst_max > layer.pitch * periodnum && inst_min < layer.pitch * (periodnum + 1);
    }
    /// Convert any [UnitSpeced]-convertible distances into [DbUnits]
    fn convert_to_db_units(&self, pt: impl Into<UnitSpeced>) -> DbUnits {
        let pt: UnitSpeced = pt.into();
        match pt {
            UnitSpeced::DbUnits(u) => u, // Return as-is
            UnitSpeced::PrimPitches(p) => {
                // Multiply by the primitive pitch in `pt`s direction
                let pitch = self.stack.prim.pitches[p.dir];
                (p.num * pitch.raw()).into()
            }
            UnitSpeced::LayerPitches(p) => {
                // LayerPitches are always in the layer's "periodic" dimension
                todo!()
            }
        }
    }
    /// Convert an [Xy] into a [raw::Point]
    fn convert_xy<T: HasUnits + Into<UnitSpeced>>(&self, xy: &Xy<T>) -> raw::Point {
        let x = self.convert_to_db_units(xy.x);
        let y = self.convert_to_db_units(xy.y);
        self.convert_point(x, y)
    }
    /// Convert a two-tuple of [DbUnits] into a [raw::Point]
    fn convert_point(&self, x: DbUnits, y: DbUnits) -> raw::Point {
        raw::Point::new(x.0, y.0)
    }
    /// Error creation helper
    fn err<T>(&self, msg: impl Into<String>) -> LayoutResult<T> {
        Err(LayoutError::Export(msg.into()))
    }
    /// Error creation helper
    /// Unwrap the [Option] `opt` if it is [Some], and return our error if not.
    fn unwrap<T>(&self, opt: Option<T>, msg: impl Into<String>) -> LayoutResult<T> {
        match opt {
            Some(val) => Ok(val),
            None => self.err(msg),
        }
    }
}