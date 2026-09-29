package {
    import flash.display.MovieClip;

    public class Part extends MovieClip {
        public var throwAt:int = 0;
        public var gotoFirst:int = 0;

        public function Part() {
            addFrameScript(0, frame1, 1, frame2, 2, frame3);
        }

        private function frame1():void {
            ran(1);
        }

        private function frame2():void {
            ran(2);
        }

        private function frame3():void {
            ran(3);
        }

        private function ran(frame:int):void {
            if (throwAt == frame) {
                throwAt = 0;
                if (gotoFirst != 0) {
                    gotoAndPlay(gotoFirst);
                }
                throw new Error(name);
            }
        }
    }
}
