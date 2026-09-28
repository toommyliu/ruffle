package {
	import flash.display.Sprite;
	import flash.events.Event;
	import flash.utils.Dictionary;

	// Entries of a weak-keyed Dictionary whose values lead back to their keys.
	// Flash Player never collects these: tracing the value keeps the key. Its
	// documentation says an object only referenced from the dictionary can be
	// collected, and Ruffle does that.
	public class Test extends Sprite {
		private var cache:Dictionary = new Dictionary(true);
		private var kept:Object = {name: "kept"};
		private var frames:int = 0;

		public function Test() {
			cache[kept] = {key: kept};

			for (var i:int = 0; i < 100; i++) {
				var part:Object = {};
				var whole:Object = {part: part};
				cache[part] = {whole: whole};
			}

			var inner:Object = {name: "inner"};
			cache[inner] = {key: inner};
			cache[kept].inner = inner;

			var a:Object = {};
			var b:Object = {};
			cache[a] = {other: b};
			cache[b] = {other: a};

			trace("entries: " + count());
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
			var garbage:Array = [];
			for (var i:int = 0; i < 20000; i++) {
				garbage.push({i: i});
			}

			frames++;
			if (frames < 30) {
				return;
			}
			removeEventListener(Event.ENTER_FRAME, onEnterFrame);

			trace("entries after collecting: " + count());
			trace("kept's value: " + (cache[kept].key === kept));
			var inner:Object = cache[kept].inner;
			trace("inner: " + inner.name + ", its value: " + (cache[inner].key === inner));
		}
	}
}
