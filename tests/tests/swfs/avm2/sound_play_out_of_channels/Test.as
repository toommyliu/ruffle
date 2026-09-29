package {
	import flash.display.Sprite;
	import flash.events.Event;
	import flash.external.ExternalInterface;
	import flash.media.SoundChannel;

	public class Test extends Sprite {
		private var completes:int = 0;
		private var frames:int = 0;

		public function Test() {
			var channels:int = 0;
			var nulls:int = 0;
			for (var i:int = 0; i < 40; i++) {
				var channel:SoundChannel = new Beep().play();
				if (channel == null) {
					nulls++;
				} else {
					channels++;
					channel.addEventListener(Event.SOUND_COMPLETE, onComplete);
				}
			}
			report("40 plays at once: " + channels + " channels, " + nulls + " null");
			addEventListener(Event.ENTER_FRAME, onEnterFrame);
		}

		private function onComplete(event:Event):void {
			completes++;
		}

		private function onEnterFrame(event:Event):void {
			frames++;
			if (frames == 48) {
				removeEventListener(Event.ENTER_FRAME, onEnterFrame);
				report("completed after 2 s: " + completes);
				report("a play after they ended: " + (new Beep().play() == null ? "null" : "a channel"));
				report("done");
			}
		}

		private function report(line:String):void {
			trace(line);
			if (ExternalInterface.available) {
				ExternalInterface.call("report", line);
			}
		}
	}
}
