use gfx::opts::Opts;
use script::dom::node::{AbstractNode, LayoutView};
use servo_net::local_image_cache::LocalImageCache;
use servo_util::deque;
use servo_util::deque::Data;
use geom::size::Size2D;
use geom::rect::Rect;
use geom::point::Point2D;
use servo_util::geometry::Au;
use servo_util::time::ProfilerChan;
use gfx::font_context::FontContext;
use layout::context::LayoutContext;
use layout::construct::{FlowConstructionResult, FlowConstructor, NoConstructionResult};
use std::task::{SingleThreaded, spawn_sched};
use std::comm;
use std::cast;
use std::cell::Cell;
use std::option;
use extra::arc::MutexArc;

struct LayoutWorkItem {
    nodes: ~[AbstractNode<LayoutView>],
}

struct WorkerInfo {
    id: uint,
    worker_port: comm::Port<Option<deque::Worker<(int,int)>>>,
    worker_chan: comm::Chan<Option<deque::Worker<(int,int)>>>,
    worker: Option<deque::Worker<(int, int)>>,
}


struct ParallelFlowBuildTask {
    work_items: ~[~[AbstractNode<LayoutView>]],
}

impl ParallelFlowBuildTask {
    pub fn new() -> ParallelFlowBuildTask {
        ParallelFlowBuildTask {
            work_items: ~[]
        }
    }

    pub fn construct_work_item(&mut self, node: AbstractNode<LayoutView>, tree_depth: uint) {
        for kid in node.children() {
            self.construct_work_item(kid, tree_depth + 1);
        }

        while self.work_items.len() < (tree_depth + 1) {
            self.work_items.push(~[]);
        }

        self.work_items[tree_depth].push(node);
    }

    pub fn build_flow(&mut self,opts: Opts, chan: ProfilerChan, 
                      local_image_cache: MutexArc<LocalImageCache>, screen_size: Size2D<Au>) {
        let worker_count:uint = 4;//rt::default_sched_threads()*2;

        let mut worker_list: ~[WorkerInfo] = ~[];
        let mut pool = deque::BufferPool::new();

        let mut deques = ~[];
        let mut stealers = ~[];
        for _ in range(0,worker_count) {
            let (mut worker,mut stealer) = pool.deque();
            deques.push(Some(worker));
            stealers.push(stealer);
        }

        for idx in range(0,worker_count) {
            let (port,ch2) = stream();
            let (port2,ch) = stream();

            let mut worker = deques[idx].take_unwrap();

            worker_list.push( WorkerInfo {
                id: idx,
                worker_chan: ch, //worker->main
                worker_port: port, //main->worker
                worker: Some(worker),
            });

            let backend = opts.render_backend.clone();
            let profile_ch = chan.clone();
            let ic = local_image_cache.clone();
            let screen_size = screen_size.clone();

            let send = (backend,profile_ch,ic,port2,ch2,stealers.clone(),idx);
            let cell = Cell::new(send);

            do spawn_sched(SingleThreaded) {
                let (backend,profile_ch,ic,port2,ch2,mut stealers,idx) = cell.take();

                let font_ctx = ~FontContext::new(backend, true, profile_ch);
                let mut lc = LayoutContext {
                    image_cache: ic,
                    font_ctx: font_ctx,
                    screen_size: Rect(Point2D(Au(0), Au(0)), screen_size),
                };
                let mut fc = FlowConstructor::init(&mut lc);

                loop {
                    let mut worker = port2.recv();
                    if worker.is_none()
                    {
                        break;
                    }
                    let mut worker = worker.take_unwrap();

                    loop{
                        match worker.pop() {
                            Some(ptr) => {
                                let list:&[AbstractNode<LayoutView>] = unsafe {
                                    cast::transmute(ptr)
                                };
                                for item  in list.iter() {
                                    fc.build(*item);
                                }
                            },
                            None => {
                                let mut steal = false;
                                for idx in range(0,worker_count) {
                                    match stealers[idx].steal() {
                                        Data(r) => {
                                            steal = true;
                                            let list:&[AbstractNode<LayoutView>] = unsafe {
                                                cast::transmute(r)
                                            };
                                            for item  in list.iter() {
                                                fc.build(*item);
                                            }
                                        },
                                        _ => {
                                        }
                                    }
                                }

                                if !steal {
                                    break;
                                }
                            }
                        }
                    }

                    ch2.send(Some(worker)); 
                }
            }
        }


        for work_l in self.work_items.rev_iter() {
            let mut send_count = 0;
            let mut idx = 0;

            let slice_size = 64;
            let mut slice_count = work_l.len()/slice_size;
            if slice_count == 0 {
                slice_count = 1;
            }

            for x in range(0,slice_count) {
                let mut end = idx + slice_size;
                if x == slice_count-1 {
                    end = work_l.len();
                }

                unsafe {
                    let t:(int,int) = cast::transmute(work_l.slice(idx,end));
                    worker_list[x % worker_count].worker.get_mut_ref().push(t);
                }
                idx = end;
                send_count = send_count + 1;

                if end >= work_l.len() {
                    break;
                }

            }

            for idx in range(0,worker_count) {
                let worker = worker_list[idx].worker.take_unwrap();
                worker_list[idx].worker_chan.send(Some(worker));
            }

            for idx in range(0,worker_count) {
                worker_list[idx].worker = worker_list[idx].worker_port.recv();
            }
        }

        for idx in range(0,worker_count) {
            worker_list[idx].worker_chan.send(None);
        }
    }

}
