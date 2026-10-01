package {
    import flash.display.Loader;
    import flash.display.MovieClip;
    import flash.events.Event;
    import flash.events.IOErrorEvent;
    import flash.events.ProgressEvent;
    import flash.external.ExternalInterface;
    import flash.net.URLRequest;
    import flash.utils.getQualifiedClassName;

    public class Test extends MovieClip {
        private var loader:Loader = new Loader();
        private var frame:int = 0;

        public function Test() {
            for each (var type:String in [Event.OPEN, Event.INIT, Event.COMPLETE, IOErrorEvent.IO_ERROR]) {
                loader.contentLoaderInfo.addEventListener(type, onEvent);
            }
            loader.contentLoaderInfo.addEventListener(ProgressEvent.PROGRESS, onProgress);
            addEventListener(Event.ENTER_FRAME, onEnterFrame);
        }

        private function onEnterFrame(e:Event):void {
            frame++;
            if (frame == 1) {
                out("close() before any load");
                close();
                out("load(child_a.swf), then close()");
                loader.load(new URLRequest("child_a.swf"));
                close();
            } else if (frame == 30) {
                out("content: " + content());
                out("load(child_a.swf), then load(child_b.swf)");
                loader.load(new URLRequest("child_a.swf"));
                loader.load(new URLRequest("child_b.swf"));
            } else if (frame == 60) {
                out("content: " + content());
                out("close() after complete");
                close();
                out("done");
                removeEventListener(Event.ENTER_FRAME, onEnterFrame);
            }
        }

        private function close():void {
            try {
                loader.close();
                out("close() returned");
            } catch (e:Error) {
                out("close() threw " + getQualifiedClassName(e) + " " + e.errorID);
            }
        }

        private function content():String {
            return loader.content == null ? "null" : getQualifiedClassName(loader.content);
        }

        private function onEvent(e:Event):void {
            out(e.type + ": " + content());
        }

        private function onProgress(e:ProgressEvent):void {
            if (e.bytesLoaded == e.bytesTotal) {
                out("progress: all bytes");
            }
        }

        private function out(line:String):void {
            trace(line);
            if (ExternalInterface.available) {
                ExternalInterface.call("report", line);
            }
        }
    }
}
