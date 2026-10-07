use photocraft_doc::roto::{BlendOp, Group, Node, NodeId, RotoMask, Transform2D};
use photocraft_doc::{Path, PathOp};

use super::curve;

/// How deep a group tree is followed (a valid mask is far shallower).
const MAX_DEPTH: usize = 64;

/// The roto mask as an ordinary path: one subpath per visible shape of two points or more under
/// `node` (the whole mask when `None`), in stacking order, with every shape's and group's
/// transform applied. A shape's blend op becomes the subpath's path op (Subtract, Intersect and
/// Difference carry over; the other ops combine). Opacity, blur, feather and invert have no
/// path equivalent and are dropped. `None` when `node` does not exist; an empty path when there
/// is nothing to export.
pub fn export_path(m: &RotoMask, node: Option<NodeId>) -> Option<Path> {
    let target = node.filter(|n| *n != m.root.id);
    if let Some(t) = target {
        m.find(t)?;
    }
    let mut path = Path::default();
    let mut chain = vec![m.root.transform];
    walk(&m.root, &mut chain, target.is_none(), target, &mut path, 0);
    Some(path)
}

fn walk(g: &Group, xf: &mut Vec<Transform2D>, inside: bool, target: Option<NodeId>, out: &mut Path, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    for n in &g.children {
        match n {
            Node::Shape(s) => {
                if (inside || target == Some(s.id)) && s.visible && s.points.len() >= 2 {
                    let mut chain = Vec::with_capacity(xf.len() + 1);
                    chain.push(s.transform);
                    chain.extend(xf.iter().rev().copied());
                    let op = match s.blend_op {
                        BlendOp::Subtract => PathOp::Subtract,
                        BlendOp::Intersect => PathOp::Intersect,
                        BlendOp::Difference => PathOp::Exclude,
                        _ => PathOp::Combine,
                    };
                    out.subpaths.push(photocraft_doc::Subpath { knots: curve::knots(s, &chain, false), closed: s.closed, op });
                }
            }
            Node::Group(c) if c.visible => {
                xf.push(c.transform);
                walk(c, xf, inside || target == Some(c.id), target, out, depth + 1);
                xf.pop();
            }
            Node::Group(_) => {}
        }
    }
}
