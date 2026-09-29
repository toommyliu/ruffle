package {
	import flash.display.Sprite;
	import flash.events.Event;
	import flash.system.System;
	import flash.utils.Dictionary;

	public class Test extends Sprite {
		private var cache:Dictionary = new Dictionary(true);
		private var frames:int = 0;

		public function Test() {
			addEventListener(Event.ENTER_FRAME, onEnterFrame);
		}

		private function count():int {
			var n:int = 0;
			for (var key:* in cache) {
				n++;
			}
			return n;
		}

		private function onEnterFrame(event:Event):void {
			frames++;
			if (frames == 10) {
				for (var i:int = 0; i < 100; i++) {
					cache[{i: i}] = i;
				}
				trace("entries: " + count());
				System.gc();
			} else if (frames == 11) {
				removeEventListener(Event.ENTER_FRAME, onEnterFrame);
				trace("entries on the next frame: " + count());
			}
		}
	}
}
